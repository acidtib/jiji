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
      log.section("Network Status:");

      // Load configuration and setup context
      ctx = await setupCommandContext(globalOptions);
      const { config, sshManagers } = ctx;

      // Show connection status for each host
      console.log(""); // Empty line
      for (const ssh of sshManagers) {
        log.remote(ssh.getHost(), ": Connected", { indent: 1 });
      }

      // Try to load topology from any available server
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
        console.log();
        log.say("No network cluster found", 1);
        log.say(
          "Run 'jiji server init' with network.enabled: true to set up private networking",
          1,
        );
        return;
      }

      // Display cluster information
      log.section("Cluster Information:");

      const stats = getTopologyStats(topology);
      log.say(`Cluster CIDR: ${topology.clusterCidr}`, 1);
      log.say(`Service Domain: ${topology.serviceDomain}`, 1);
      log.say(`Discovery: ${topology.discovery}`, 1);
      log.say(`Server Count: ${stats.serverCount}`, 1);
      log.say(`Cluster Age: ${stats.clusterAge}`, 1);

      // Display server status
      log.section("Server Status:");

      for (const server of topology.servers) {
        const ssh = sshManagers.find((s) => s.getHost() === server.hostname);

        if (!ssh) {
          await log.hostBlock(
            `${server.hostname} (${server.id})`,
            async () => {
              log.say("Status: OFFLINE (SSH connection failed)", 2);
            },
            { indent: 1 },
          );
          continue;
        }

        await log.hostBlock(
          server.hostname,
          async () => {
            log.say(`Server ID: ${server.id}`, 2);
            log.say(`Subnet: ${server.subnet}`, 2);
            log.say(`WireGuard IP: ${server.wireguardIp}`, 2);
            log.say(`Management IP: ${server.managementIp}`, 2);

            // Check WireGuard status
            const wgStatus = await getWireGuardStatus(ssh);
            if (wgStatus.up) {
              log.say(
                `WireGuard: UP (${wgStatus.peers} peers connected)`,
                2,
              );
            } else if (wgStatus.exists) {
              log.say(
                "WireGuard: DOWN (interface exists but not active)",
                2,
              );
            } else {
              log.say("WireGuard: NOT CONFIGURED", 2);
            }

            // Check Corrosion status
            if (topology.discovery === "corrosion") {
              const corrRunning = await isCorrosionRunning(ssh);
              if (corrRunning) {
                log.say("Corrosion: RUNNING", 2);
              } else {
                log.say("Corrosion: NOT RUNNING", 2);
              }
            }

            // Check DNS status
            const dnsRunning = await isCoreDNSRunning(ssh);
            if (dnsRunning) {
              log.say("DNS: RUNNING", 2);
            } else {
              log.say("DNS: NOT RUNNING", 2);
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
                  log.say("Containers:", 2);
                  for (const container of containers) {
                    let proxyInfo = "";
                    if (proxyDetails && proxyDetails.has(container.service)) {
                      const details = proxyDetails.get(container.service)!;
                      const protocol = details.tls ? "https" : "http";
                      // Handle multiple hosts (comma-separated)
                      const hosts = details.host.split(",").map((h) =>
                        h.trim()
                      );
                      const hostUrls = hosts.map((h) => `${protocol}://${h}`)
                        .join(", ");
                      proxyInfo = ` -> ${hostUrls}`;
                    }
                    log.say(
                      `${container.service}: ${container.ip} (${container.domain})${proxyInfo}`,
                      3,
                    );
                  }
                } else {
                  log.say("Containers: none", 2);
                }
              } catch (error) {
                log.say(`Containers: failed to query (${error})`, 2);
              }
            }
          },
          { indent: 1 },
        );
      }

      console.log();
      log.say("Network status check complete");
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
