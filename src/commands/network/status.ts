/**
 * Network status command
 *
 * Shows the status of the private network including servers,
 * WireGuard connections, and container registrations.
 */

import { Command } from "@cliffy/command";
import { Configuration } from "../../lib/configuration.ts";
import { getTopologyStats, loadTopology } from "../../lib/network/topology.ts";
import { setupSSHConnections } from "../../utils/ssh.ts";
import { getWireGuardStatus } from "../../lib/network/wireguard.ts";
import {
  isCorrosionRunning,
  queryServiceContainers,
} from "../../lib/network/corrosion.ts";
import { isCoreDNSRunning } from "../../lib/network/dns.ts";
import { log } from "../../utils/logger.ts";
import type { GlobalOptions } from "../../types.ts";

export const statusCommand = new Command()
  .description("Show network status")
  .action(async (options) => {
    try {
      await log.group("Network Status", async () => {
        // Load topology
        const topology = await loadTopology();

        if (!topology) {
          log.info("No network topology found", "network");
          log.info(
            "Run 'jiji server bootstrap' with network.enabled: true to set up private networking",
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

        // Cast options to GlobalOptions
        const globalOptions = options as unknown as GlobalOptions;

        // Load configuration for SSH settings
        const config = await Configuration.load(
          globalOptions.environment,
          globalOptions.configFile,
        );

        // Connect to servers
        const hostnames = topology.servers.map((s) => s.hostname);
        const { managers: sshManagers } = await setupSSHConnections(
          hostnames,
          {
            user: config.ssh.user,
            port: config.ssh.port,
            proxy: config.ssh.proxy,
            proxy_command: config.ssh.proxyCommand,
            keys: config.ssh.allKeys.length > 0
              ? config.ssh.allKeys
              : undefined,
            keyData: config.ssh.keyData,
            keysOnly: config.ssh.keysOnly,
            dnsRetries: config.ssh.dnsRetries,
          },
          { allowPartialConnection: true },
        );

        try {
          // Check status of each server
          for (const server of topology.servers) {
            const ssh = sshManagers.find((s) =>
              s.getHost() === server.hostname
            );

            if (!ssh) {
              log.info(`\n${server.hostname} (${server.id})`, "network");
              log.error("  Status: OFFLINE (SSH connection failed)", "network");
              continue;
            }

            log.info(`\n${server.hostname} (${server.id})`, "network");
            log.info(`  Subnet: ${server.subnet}`, "network");
            log.info(`  WireGuard IP: ${server.wireguardIp}`, "network");
            log.info(`  Management IP: ${server.managementIp}`, "network");

            // Check WireGuard status
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

            // Check Corrosion status
            if (topology.discovery === "corrosion") {
              const corrRunning = await isCorrosionRunning(ssh);
              if (corrRunning) {
                log.success("  Corrosion: RUNNING", "network");
              } else {
                log.error("  Corrosion: NOT RUNNING", "network");
              }
            }

            // Check DNS status
            const dnsRunning = await isCoreDNSRunning(ssh);
            if (dnsRunning) {
              log.success("  DNS: RUNNING", "network");
            } else {
              log.error("  DNS: NOT RUNNING", "network");
            }

            // Query containers on this server (if Corrosion is running)
            if (topology.discovery === "corrosion") {
              try {
                // Get all services
                const services = config.getServiceNames();
                const containers: Array<{ service: string; ip: string }> = [];

                for (const serviceName of services) {
                  const ips = await queryServiceContainers(ssh, serviceName);
                  for (const ip of ips) {
                    // Check if IP belongs to this server's subnet
                    if (
                      ip.startsWith(
                        server.subnet.split("/")[0].substring(
                          0,
                          server.subnet.lastIndexOf(".") - 1,
                        ),
                      )
                    ) {
                      containers.push({ service: serviceName, ip });
                    }
                  }
                }

                if (containers.length > 0) {
                  log.info("  Containers:", "network");
                  for (const container of containers) {
                    log.info(
                      `    - ${container.service}: ${container.ip}`,
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
        } finally {
          // Clean up SSH connections
          for (const ssh of sshManagers) {
            try {
              ssh.dispose();
            } catch (error) {
              log.debug(`Failed to dispose SSH connection: ${error}`, "ssh");
            }
          }
        }
      });
    } catch (error) {
      const errorMessage = error instanceof Error
        ? error.message
        : String(error);
      log.error("Failed to get network status:", "network");
      log.error(errorMessage, "network");
      Deno.exit(1);
    }
  });
