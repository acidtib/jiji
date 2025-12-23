/**
 * Service for managing kamal-proxy installation and configuration
 */

import type { ContainerEngine } from "../configuration/builder.ts";
import type { Configuration } from "../configuration.ts";
import type { ServiceConfiguration } from "../configuration/service.ts";
import type { SSHManager } from "../../utils/ssh.ts";
import { extractAppPort, ProxyCommands } from "../../utils/proxy.ts";
import { createServerAuditLogger } from "../../utils/audit.ts";
import { getDnsServerForHost } from "../../utils/network_helpers.ts";
import { getContainerIp } from "./container_registry.ts";
import { log } from "../../utils/logger.ts";
import type { ProxyConfigResult, ProxyInstallResult } from "../../types.ts";

/**
 * Service for managing kamal-proxy across multiple hosts
 */
export class ProxyService {
  constructor(
    private engine: ContainerEngine,
    private config: Configuration,
    private sshManagers: SSHManager[],
  ) {}

  /**
   * Ensure kamal-proxy is installed and running on specified hosts
   *
   * @param hosts Set of hostnames where proxy should be installed
   * @returns Array of installation results
   */
  async ensureProxyOnHosts(
    hosts: Set<string>,
  ): Promise<ProxyInstallResult[]> {
    const results: ProxyInstallResult[] = [];

    for (const host of hosts) {
      const hostSsh = this.sshManagers.find((ssh) => ssh.getHost() === host);
      if (!hostSsh) {
        results.push({
          host,
          success: false,
          error: "SSH connection not found",
        });
        continue;
      }

      await log.hostBlock(host, async () => {
        try {
          const proxyCmd = new ProxyCommands(this.engine, hostSsh);

          // Ensure network exists
          await proxyCmd.ensureNetwork();

          // Check if proxy is already running
          const isRunning = await proxyCmd.isRunning();

          if (isRunning) {
            const version = await proxyCmd.getVersion();
            log.say(
              `└── kamal-proxy already running on ${host} (version: ${
                version || "unknown"
              })`,
              2,
            );
            results.push({
              host,
              success: true,
              message: "Already running",
              version: version || undefined,
            });
          } else {
            // Get DNS server from network topology
            let dnsServer: string | undefined;
            if (this.config.network.enabled) {
              dnsServer = await getDnsServerForHost(
                hostSsh,
                host,
                this.config.network.enabled,
              );
            }

            // Boot the proxy
            log.say(`Booting kamal-proxy on ${host}...`, 2);
            await proxyCmd.boot({ dnsServer });

            const version = await proxyCmd.getVersion();
            log.say(
              `kamal-proxy started on ${host} (version: ${
                version || "unknown"
              })`,
              2,
            );
            results.push({
              host,
              success: true,
              message: "Started",
              version: version || undefined,
            });

            // Log to audit
            const hostLogger = createServerAuditLogger(
              hostSsh,
              this.config.project,
            );
            await hostLogger.logProxyEvent(
              "boot",
              "success",
              `kamal-proxy ${version || "unknown"} started`,
            );
          }
        } catch (error) {
          const errorMessage = error instanceof Error
            ? error.message
            : String(error);
          log.error(
            `Failed to install proxy on ${host}: ${errorMessage}`,
            2,
          );
          results.push({
            host,
            success: false,
            error: errorMessage,
          });

          // Log failure to audit
          const hostLogger = createServerAuditLogger(
            hostSsh,
            this.config.project,
          );
          await hostLogger.logProxyEvent("boot", "failed", errorMessage);
        }
      }, { indent: 1 });
    }

    // Summary
    const failCount = results.filter((r) => !r.success).length;

    if (failCount > 0) {
      log.error(
        `kamal-proxy installation failed on ${failCount} host(s)`,
        1,
      );
    }

    return results;
  }

  /**
   * Configure proxy for a service on a specific host
   *
   * @param service Service configuration
   * @param host Hostname
   * @param ssh SSH manager for the host
   * @returns Configuration result
   */
  async configureServiceProxy(
    service: ServiceConfiguration,
    host: string,
    ssh: SSHManager,
  ): Promise<ProxyConfigResult> {
    const proxyConfig = service.proxy;
    if (!proxyConfig || !proxyConfig.enabled) {
      return {
        service: service.name,
        host,
        success: false,
        error: "Proxy not enabled for service",
      };
    }

    try {
      log.say(`├── Configuring ${service.name} on proxy at ${host}`, 2);

      const proxyCmd = new ProxyCommands(this.engine, ssh);
      const containerName = service.getContainerName();
      const appPort = extractAppPort(service.ports);

      // Get container IP to avoid DNS caching issues
      let containerIp: string | undefined;
      try {
        const ip = await getContainerIp(ssh, containerName, this.engine);
        if (ip) {
          containerIp = ip;
          log.debug(
            `Using container IP ${containerIp} for ${service.name}`,
            "proxy",
          );
        }
      } catch (error) {
        log.debug(
          `Could not get container IP for ${service.name}, will use DNS name: ${error}`,
          "proxy",
        );
      }

      await proxyCmd.deploy(
        service.name,
        containerName,
        proxyConfig,
        appPort,
        this.config.project,
        containerIp,
      );

      const hostsStr = proxyConfig.host || proxyConfig.hosts.join(", ");
      log.say(
        `└── ${service.name} configured (${hostsStr}, port ${appPort})`,
        2,
      );

      // Log to audit
      const hostLogger = createServerAuditLogger(ssh, this.config.project);
      await hostLogger.logProxyEvent(
        "deploy",
        "success",
        `${service.name} -> ${hostsStr}:${appPort} (SSL: ${proxyConfig.ssl})`,
      );

      return {
        service: service.name,
        host,
        success: true,
        message: `Configured at ${hostsStr}:${appPort}`,
      };
    } catch (error) {
      const errorMessage = error instanceof Error
        ? error.message
        : String(error);
      log.error(
        `Failed to configure ${service.name} on proxy at ${host}: ${errorMessage}`,
        "proxy",
      );

      // Fetch and display service container logs to help diagnose the issue
      const containerName = service.getContainerName();
      log.info(
        `Fetching logs from ${containerName} to diagnose the issue...`,
        "proxy",
      );

      const logsCmd = `${this.engine} logs --tail 50 ${containerName} 2>&1`;
      const logsResult = await ssh.executeCommand(logsCmd);

      if (logsResult.success && logsResult.stdout.trim()) {
        log.error(`Container logs for ${containerName}:`, "proxy");
        // Split logs into lines and display each with error level
        const logLines = logsResult.stdout.trim().split("\n");
        for (const line of logLines) {
          log.error(`  ${line}`, "proxy");
        }
      } else {
        log.warn(`No logs available for ${containerName}`, "proxy");
      }

      return {
        service: service.name,
        host,
        success: false,
        error: errorMessage,
      };
    }
  }

  /**
   * Configure proxy for all services that need it
   *
   * @param services Services with proxy configuration
   * @returns Array of configuration results
   */
  async configureProxyForServices(
    services: ServiceConfiguration[],
  ): Promise<ProxyConfigResult[]> {
    const results: ProxyConfigResult[] = [];

    for (const service of services) {
      log.say(`- Configuring proxy for ${service.name}`, 0);

      for (const server of service.servers) {
        const host = server.host;
        const hostSsh = this.sshManagers.find((ssh) => ssh.getHost() === host);

        if (!hostSsh) {
          await log.hostBlock(host, async () => {
            log.say(`└── Skipping ${service.name} on unreachable host`, 2);
            results.push({
              service: service.name,
              host,
              success: false,
              error: "SSH connection not found",
            });
          }, { indent: 1 });
          continue;
        }

        await log.hostBlock(host, async () => {
          const result = await this.configureServiceProxy(
            service,
            host,
            hostSsh,
          );
          results.push(result);
        }, { indent: 1 });
      }
    }

    return results;
  }

  /**
   * Wait for a service to become healthy in kamal-proxy
   *
   * @param service Service configuration
   * @param host Hostname where the proxy is running
   * @param ssh SSH manager for the host
   * @param timeoutMs Maximum time to wait in milliseconds (default: 30000)
   * @returns true if service becomes healthy, false if timeout
   */
  async waitForServiceHealthy(
    service: ServiceConfiguration,
    host: string,
    ssh: SSHManager,
    timeoutMs: number = 30000,
  ): Promise<boolean> {
    const proxyConfig = service.proxy;
    if (!proxyConfig || !proxyConfig.enabled) {
      return true; // No proxy, nothing to wait for
    }

    // Use deploy_timeout from healthcheck config if available
    const configuredTimeout = proxyConfig.healthcheck?.deploy_timeout;
    if (configuredTimeout) {
      // Parse timeout string (e.g., "30s", "1m") to milliseconds
      const match = configuredTimeout.match(/^(\d+)(s|m)$/);
      if (match) {
        const value = parseInt(match[1], 10);
        const unit = match[2];
        timeoutMs = unit === "s" ? value * 1000 : value * 60 * 1000;
      }
    }

    log.say(
      `├── Waiting for ${service.name} to pass health checks (timeout: ${timeoutMs}ms)...`,
      2,
    );

    const startTime = Date.now();
    const proxyCmd = new ProxyCommands(this.engine, ssh);
    const checkInterval = 2000; // Check every 2 seconds

    while (Date.now() - startTime < timeoutMs) {
      try {
        const serviceDetails = await proxyCmd.getServiceDetails();
        const details = serviceDetails.get(service.name);

        if (details) {
          log.debug(
            `${service.name} state: ${details.state}, target: ${details.target}`,
            "proxy",
          );

          // Check if service is in a healthy state
          // kamal-proxy uses "deployed" or "running" state for healthy services
          if (details.state === "deployed" || details.state === "running") {
            log.say(
              `├── ${service.name} passed health checks`,
              2,
            );
            return true;
          }

          // If state is "error" or "unhealthy", fail immediately
          if (details.state === "error" || details.state === "unhealthy") {
            log.say(
              `├── ${service.name} health check failed with state: ${details.state}`,
              2,
            );
            return false;
          }
        }
      } catch (error) {
        log.debug(
          `Error checking service health: ${error}`,
          "proxy",
        );
      }

      await new Promise((resolve) => setTimeout(resolve, checkInterval));
    }

    log.say(
      `├── ${service.name} did not become healthy within ${timeoutMs}ms`,
      2,
    );
    return false;
  }

  /**
   * Get hosts that need proxy based on service configurations
   *
   * @param services Services to check
   * @param connectedHosts Hosts that are currently connected
   * @returns Set of hostnames that need proxy
   */
  static getHostsNeedingProxy(
    services: ServiceConfiguration[],
    connectedHosts: string[],
  ): Set<string> {
    const proxyHosts = new Set<string>();

    const servicesWithProxy = services.filter((s) => s.proxy?.enabled);

    for (const service of servicesWithProxy) {
      for (const server of service.servers) {
        const host = server.host;
        if (connectedHosts.includes(host)) {
          proxyHosts.add(host);
        }
      }
    }

    return proxyHosts;
  }
}
