/**
 * Service for managing kamal-proxy installation and configuration
 */

import type { ContainerEngine } from "../configuration/builder.ts";
import type { Configuration } from "../configuration.ts";
import type { ServiceConfiguration } from "../configuration/service.ts";
import type { SSHManager } from "../../utils/ssh.ts";
import { ProxyCommands } from "../../utils/proxy.ts";
import type { ProxySslCerts } from "../configuration/proxy.ts";
import { createServerAuditLogger } from "../../utils/audit.ts";
import { getDnsServerForHost } from "../../utils/network_helpers.ts";
import { getContainerIp } from "./container_registry.ts";
import { log } from "../../utils/logger.ts";
import type { ProxyConfigResult, ProxyInstallResult } from "../../types.ts";

/**
 * Write PEM certificate and key files to the remote host via SFTP.
 * Files are written to .jiji/certs/{project}/{serviceName}/ and
 * returns the container-internal paths (mounted at /jiji-certs).
 */
async function writeCertFiles(
  ssh: SSHManager,
  project: string,
  serviceName: string,
  certPem: string,
  keyPem: string,
): Promise<{ cert: string; key: string }> {
  const remoteDir = `.jiji/certs/${project}/${serviceName}`;
  const remoteCertPath = `${remoteDir}/cert.pem`;
  const remoteKeyPath = `${remoteDir}/key.pem`;

  // Create remote directory
  const mkdirResult = await ssh.executeCommand(`mkdir -p ${remoteDir}`);
  if (!mkdirResult.success) {
    throw new Error(
      `Failed to create cert directory ${remoteDir}: ${mkdirResult.stderr}`,
    );
  }

  // Write cert.pem via temp file
  const tempCert = await Deno.makeTempFile({ suffix: ".pem" });
  try {
    await Deno.writeTextFile(tempCert, certPem);
    await ssh.uploadFile(tempCert, remoteCertPath);
  } finally {
    await Deno.remove(tempCert).catch(() => {});
  }

  // Write key.pem via temp file
  const tempKey = await Deno.makeTempFile({ suffix: ".pem" });
  try {
    await Deno.writeTextFile(tempKey, keyPem);
    await ssh.uploadFile(tempKey, remoteKeyPath);
  } finally {
    await Deno.remove(tempKey).catch(() => {});
  }

  // Restrict key file permissions
  await ssh.executeCommand(`chmod 600 ${remoteKeyPath}`);

  return {
    cert: `/jiji-certs/${project}/${serviceName}/cert.pem`,
    key: `/jiji-certs/${project}/${serviceName}/key.pem`,
  };
}

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
   * Fetch and log container logs for debugging
   */
  private async fetchAndLogContainerLogs(
    ssh: SSHManager,
    containerName: string,
  ): Promise<void> {
    log.info(
      `Fetching logs from ${containerName} to diagnose the issue...`,
      "proxy",
    );

    const logsCmd = `${this.engine} logs --tail 50 ${containerName} 2>&1`;
    const logsResult = await ssh.executeCommand(logsCmd);

    if (logsResult.success && logsResult.stdout.trim()) {
      log.error(`Container logs for ${containerName}:`, "proxy");
      const logLines = logsResult.stdout.trim().split("\n");
      for (const line of logLines) {
        log.error(`  ${line}`, "proxy");
      }
    } else {
      log.warn(`No logs available for ${containerName}`, "proxy");
    }
  }

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
          const needsReboot = isRunning && !(await proxyCmd.hasCertsMount());

          if (isRunning && !needsReboot) {
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

            if (needsReboot) {
              log.say(
                `Rebooting kamal-proxy on ${host} to add certs volume mount...`,
                2,
              );
              await proxyCmd.run({ dnsServer });
              await proxyCmd.waitForReady();
            } else {
              log.say(`Booting kamal-proxy on ${host}...`, 2);
              await proxyCmd.boot({ dnsServer });
            }

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
              message: needsReboot ? "Rebooted" : "Started",
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
              `kamal-proxy ${version || "unknown"} ${
                needsReboot ? "rebooted" : "started"
              }`,
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
   * @param envVars Resolved environment variables for secret lookups
   * @returns Configuration result
   */
  async configureServiceProxy(
    service: ServiceConfiguration,
    host: string,
    ssh: SSHManager,
    envVars: Record<string, string> = {},
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
      const proxyCmd = new ProxyCommands(this.engine, ssh);
      const containerName = service.getContainerName();

      // Get container IP once for all targets
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

      // Deploy each target
      const targets = proxyConfig.targets;
      const targetResults: string[] = [];
      const failedTargets: Array<{ port: number; error: string }> = [];

      for (const target of targets) {
        const appPort = target.app_port;
        const targetServiceName =
          `${this.config.project}-${service.name}-${appPort}`;

        log.say(
          `├── Configuring ${targetServiceName} on proxy at ${host}`,
          2,
        );

        try {
          // Resolve custom TLS cert paths if configured
          let certPaths: { cert: string; key: string } | undefined;
          if (target.ssl && typeof target.ssl === "object") {
            const certs = target.ssl as ProxySslCerts;
            const certPem = envVars[certs.certificate_pem];
            const keyPem = envVars[certs.private_key_pem];
            if (!certPem) {
              throw new Error(
                `Secret '${certs.certificate_pem}' not found in environment for TLS certificate`,
              );
            }
            if (!keyPem) {
              throw new Error(
                `Secret '${certs.private_key_pem}' not found in environment for TLS private key`,
              );
            }
            log.debug(
              `Writing TLS certs for ${targetServiceName} on ${host}`,
              "proxy",
            );
            certPaths = await writeCertFiles(
              ssh,
              this.config.project,
              `${service.name}-${appPort}`,
              certPem,
              keyPem,
            );
          }

          await proxyCmd.deployTarget(
            targetServiceName,
            containerName,
            target,
            appPort,
            this.config.project,
            containerIp,
            certPaths,
          );

          const hostsStr = target.host ||
            (target.hosts || []).join(", ");
          targetResults.push(`${hostsStr}:${appPort}`);

          log.say(
            `└── ${targetServiceName} configured (${hostsStr}, port ${appPort})`,
            2,
          );
        } catch (error) {
          const errorMessage = error instanceof Error
            ? error.message
            : String(error);
          failedTargets.push({
            port: appPort,
            error: errorMessage,
          });
          log.error(
            `Failed to configure ${targetServiceName}: ${errorMessage}`,
            "proxy",
          );

          // Fetch container logs to help diagnose the issue
          await this.fetchAndLogContainerLogs(ssh, containerName);
        }
      }

      // Log to audit
      const hostLogger = createServerAuditLogger(ssh, this.config.project);

      if (failedTargets.length === 0) {
        await hostLogger.logProxyEvent(
          "deploy",
          "success",
          `${service.name} -> ${targetResults.join(", ")}`,
        );

        return {
          service: service.name,
          host,
          success: true,
          message: `Configured: ${targetResults.join(", ")}`,
        };
      } else {
        const errorMsg =
          `Failed to deploy ${failedTargets.length} target(s): ` +
          failedTargets.map((f) => `port ${f.port} (${f.error})`).join(", ");

        await hostLogger.logProxyEvent(
          "deploy",
          "failed",
          errorMsg,
        );

        return {
          service: service.name,
          host,
          success: false,
          error: errorMsg,
        };
      }
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
      await this.fetchAndLogContainerLogs(ssh, containerName);

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
   * @param envVars Resolved environment variables for secret lookups
   * @returns Array of configuration results
   */
  async configureProxyForServices(
    services: ServiceConfiguration[],
    envVars: Record<string, string> = {},
  ): Promise<ProxyConfigResult[]> {
    const results: ProxyConfigResult[] = [];

    for (const service of services) {
      log.say(`- Configuring proxy for ${service.name}`, 1);

      // Get resolved servers for this service
      const resolvedServers = this.config.getResolvedServersForService(
        service.name,
      );

      for (const server of resolvedServers) {
        const host = server.host;
        const hostSsh = this.sshManagers.find((ssh) => ssh.getHost() === host);

        if (!hostSsh) {
          await log.hostBlock(host, () => {
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
            envVars,
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
    _host: string,
    ssh: SSHManager,
    timeoutMs: number = 30000,
  ): Promise<boolean> {
    const proxyConfig = service.proxy;
    if (!proxyConfig || !proxyConfig.enabled) {
      return true; // No proxy, nothing to wait for
    }

    const targets = proxyConfig.targets;

    // Wait for each target sequentially
    for (const target of targets) {
      const appPort = target.app_port;
      const targetServiceName =
        `${this.config.project}-${service.name}-${appPort}`;

      // Use target-specific timeout if configured
      let targetTimeout = timeoutMs;
      const configuredTimeout = target.healthcheck?.deploy_timeout;
      if (configuredTimeout) {
        targetTimeout = this.parseTimeout(configuredTimeout);
      }

      log.say(
        `├── Waiting for ${targetServiceName} to pass health checks (timeout: ${targetTimeout}ms)...`,
        2,
      );

      const healthy = await this.waitForTargetHealthy(
        targetServiceName,
        ssh,
        targetTimeout,
      );

      if (!healthy) {
        log.say(
          `├── ${targetServiceName} did not become healthy within ${targetTimeout}ms`,
          2,
        );
        return false;
      }

      log.say(`├── ${targetServiceName} passed health checks`, 2);
    }

    return true;
  }

  /**
   * Wait for a specific target to become healthy
   */
  private async waitForTargetHealthy(
    serviceName: string,
    ssh: SSHManager,
    timeoutMs: number,
  ): Promise<boolean> {
    const startTime = Date.now();
    const proxyCmd = new ProxyCommands(this.engine, ssh);
    const checkInterval = 2000; // Check every 2 seconds

    while (Date.now() - startTime < timeoutMs) {
      try {
        const serviceDetails = await proxyCmd.getServiceDetails();
        const details = serviceDetails.get(serviceName);

        if (details) {
          log.debug(
            `${serviceName} state: ${details.state}, target: ${details.target}`,
            "proxy",
          );

          // Check if service is in a healthy state
          // kamal-proxy uses "deployed" or "running" state for healthy services
          if (details.state === "deployed" || details.state === "running") {
            return true;
          }

          // If state is "error" or "unhealthy", fail immediately
          if (details.state === "error" || details.state === "unhealthy") {
            log.say(
              `├── ${serviceName} health check failed with state: ${details.state}`,
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

    return false;
  }

  /**
   * Parse timeout string to milliseconds
   */
  private parseTimeout(timeoutStr: string): number {
    const match = timeoutStr.match(/^(\d+)(s|m)$/);
    if (match) {
      const value = parseInt(match[1], 10);
      const unit = match[2];
      return unit === "s" ? value * 1000 : value * 60 * 1000;
    }
    return 30000; // Default 30s
  }

  /**
   * Get hosts that need proxy based on service configurations
   *
   * @param config Configuration object
   * @param services Services to check
   * @param connectedHosts Hosts that are currently connected
   * @returns Set of hostnames that need proxy
   */
  static getHostsNeedingProxy(
    config: Configuration,
    services: ServiceConfiguration[],
    connectedHosts: string[],
  ): Set<string> {
    const proxyHosts = new Set<string>();

    const servicesWithProxy = services.filter((s) => s.proxy?.enabled);

    for (const service of servicesWithProxy) {
      const resolvedServers = config.getResolvedServersForService(service.name);
      for (const server of resolvedServers) {
        const host = server.host;
        if (connectedHosts.includes(host)) {
          proxyHosts.add(host);
        }
      }
    }

    return proxyHosts;
  }
}
