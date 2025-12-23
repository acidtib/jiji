/**
 * Network status command
 *
 * Shows the status of the private network including servers,
 * WireGuard connections, and container registrations.
 */

import { Command } from "@cliffy/command";
import {
  cleanupSSHConnections,
  setupCommandContext,
} from "../../utils/command_helpers.ts";
import { handleCommandError } from "../../utils/error_handler.ts";
import { getTopologyStats, loadTopology } from "../../lib/network/topology.ts";
import { getWireGuardStatus } from "../../lib/network/wireguard.ts";
import {
  isCorrosionRunning,
  queryServiceContainers,
} from "../../lib/network/corrosion.ts";
import { isCoreDNSRunning } from "../../lib/network/dns.ts";
import { log } from "../../utils/logger.ts";
import type { GlobalOptions } from "../../types.ts";
import { ProxyCommands } from "../../utils/proxy.ts";

export const statusCommand = new Command()
  .description("Show network status")
  .action(async (options) => {
    const globalOptions = options as unknown as GlobalOptions;
    let ctx: Awaited<ReturnType<typeof setupCommandContext>> | undefined;

    try {
      await log.group("Network Status", async () => {
        ctx = await setupCommandContext(globalOptions);
        const { config, sshManagers } = ctx;

        let topology = null;
        for (const ssh of sshManagers) {
          try {
            topology = await loadTopology(ssh);
            if (topology) break;
          } catch {
            continue;
          }
        }

        if (!topology) {
          log.info("No network cluster found in Corrosion", "network");
          log.info(
            "Run 'jiji server init' with network.enabled: true to set up private networking",
            "network",
          );
          return;
        }

        // Display topology info
        const stats = getTopologyStats(topology);
        log.info(`Cluster CIDR: ${topology.clusterCidr}`, "network");
        log.info(`Service Domain: ${topology.serviceDomain}`, "network");
        log.info(`Discovery: ${topology.discovery}`, "network");
        log.info(`Server Count: ${stats.serverCount}`, "network");
        log.info(`Cluster Age: ${stats.clusterAge}`, "network");
        log.info("", "network");

        for (const server of topology.servers) {
          const ssh = sshManagers.find((s) => s.getHost() === server.hostname);

          if (!ssh) {
            log.info(`  ${server.hostname} (${server.id})`, "network");
            log.error("  Status: OFFLINE (SSH connection failed)", "network");
            continue;
          }

          log.info(`  ${server.hostname} (${server.id})`, "network");
          log.info(`  Subnet: ${server.subnet}`, "network");
          log.info(`  WireGuard IP: ${server.wireguardIp}`, "network");
          log.info(`  Management IP: ${server.managementIp}`, "network");

          const wgStatus = await getWireGuardStatus(ssh);
          if (wgStatus.up) {
            log.success(
              `  WireGuard: UP (${wgStatus.peers} peers connected)`,
              "network",
            );
          } else if (wgStatus.exists) {
            log.warn(
              "  WireGuard: DOWN (interface exists but not active)",
              "network",
            );
          } else {
            log.error("  WireGuard: NOT CONFIGURED", "network");
          }

          if (topology.discovery === "corrosion") {
            const corrRunning = await isCorrosionRunning(ssh);
            if (corrRunning) {
              log.success("  Corrosion: RUNNING", "network");
            } else {
              log.error("  Corrosion: NOT RUNNING", "network");
            }
          }

          const dnsRunning = await isCoreDNSRunning(ssh);
          if (dnsRunning) {
            log.success("  DNS: RUNNING", "network");
          } else {
            log.error("  DNS: NOT RUNNING", "network");
          }

          // Query containers on this server (if Corrosion is running)
          if (topology.discovery === "corrosion") {
            try {
              const services = config.getServiceNames();
              const containers: Array<
                { service: string; ip: string; domain: string }
              > = [];

              for (const serviceName of services) {
                const ips = await queryServiceContainers(ssh, serviceName);
                for (const ip of ips) {
                  if (
                    ip.startsWith(
                      server.subnet.split("/")[0].substring(
                        0,
                        server.subnet.lastIndexOf(".") - 1,
                      ),
                    )
                  ) {
                    const domain =
                      `${config.project}-${serviceName}.${topology.serviceDomain}`;
                    containers.push({ service: serviceName, ip, domain });
                  }
                }
              }

              // Get kamal-proxy service details if proxy is running
              let proxyDetails:
                | Map<
                  string,
                  {
                    host: string;
                    path: string;
                    target: string;
                    state: string;
                    tls: boolean;
                  }
                >
                | undefined;
              try {
                const proxyCommands = new ProxyCommands(
                  config.builder.engine,
                  ssh,
                );
                const isRunning = await proxyCommands.isRunning();
                if (isRunning) {
                  proxyDetails = await proxyCommands.getServiceDetails();
                }
              } catch (_error) {
                // Silently ignore proxy errors - it might not be installed
              }

              if (containers.length > 0) {
                log.info("  Containers:", "network");
                for (const container of containers) {
                  let proxyInfo = "";
                  if (proxyDetails && proxyDetails.has(container.service)) {
                    const details = proxyDetails.get(container.service)!;
                    const protocol = details.tls ? "https" : "http";
                    // Handle multiple hosts (comma-separated)
                    const hosts = details.host.split(",").map((h) => h.trim());
                    const hostUrls = hosts.map((h) => `${protocol}://${h}`)
                      .join(
                        ", ",
                      );
                    proxyInfo = ` -> ${hostUrls}`;
                  }
                  log.info(
                    `    - ${container.service}: ${container.ip} (${container.domain})${proxyInfo}`,
                    "network",
                  );
                }
              } else {
                log.info("  Containers: none", "network");
              }
            } catch (error) {
              log.warn(`  Containers: failed to query (${error})`, "network");
            }
          }
        }

        log.info("\n", "network");
        log.success("Network status check complete", "network");
      });
    } catch (error) {
      await handleCommandError(error, {
        operation: "Network status",
        component: "network",
        sshManagers: ctx?.sshManagers,
        projectName: ctx?.config?.project,
        targetHosts: ctx?.targetHosts,
      });
    } finally {
      if (ctx?.sshManagers) {
        cleanupSSHConnections(ctx.sshManagers);
      }
    }
  });
