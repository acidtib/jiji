/**
 * Network status command
 *
 * Shows the status of the private network including servers,
 * WireGuard connections, and container registrations.
 */

import { Command } from "@cliffy/command";
import {
  cleanupSSHConnections,
  displayCommandHeader,
  setupCommandContext,
} from "../../utils/command_helpers.ts";
import { handleCommandError } from "../../utils/error_handler.ts";
import { getTopologyStats, loadTopology } from "../../lib/network/topology.ts";
import { getWireGuardStatus } from "../../lib/network/wireguard.ts";
import {
  isCorrosionRunning,
  queryServerServiceContainerDetails,
} from "../../lib/network/corrosion.ts";
import { isJijiDnsRunning } from "../../lib/network/dns.ts";
import {
  hasContainerForwardRules,
  isUfwActive,
} from "../../lib/network/firewall.ts";
import { log } from "../../utils/logger.ts";
import type { GlobalOptions } from "../../types.ts";
import { ProxyCommands } from "../../utils/proxy.ts";

export const statusCommand = new Command()
  .description("Show network status")
  .action(async (options) => {
    const globalOptions = options as unknown as GlobalOptions;
    let ctx: Awaited<ReturnType<typeof setupCommandContext>> | undefined;

    try {
      // Setup command context (load config and establish SSH connections)
      ctx = await setupCommandContext(globalOptions);
      const { config, sshManagers } = ctx;

      // Display standardized command header
      displayCommandHeader("Network Status:", config, sshManagers);

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
      log.say(`- CIDR: ${topology.clusterCidr}`, 1);
      log.say(`- Service domain: ${topology.serviceDomain}`, 1);
      log.say(`- Discovery: ${topology.discovery}`, 1);
      log.say(`- Servers: ${stats.serverCount}`, 1);
      log.say(`- Age: ${stats.clusterAge}`, 1);

      // Display server status
      log.section("Server Status:");

      for (const server of topology.servers) {
        const ssh = sshManagers.find((s) => s.getHost() === server.hostname);

        if (!ssh) {
          await log.hostBlock(
            `${server.hostname} (${server.id})`,
            () => {
              log.say(`└── Status: OFFLINE (SSH connection failed)`, 2);
            },
            { indent: 1 },
          );
          continue;
        }

        await log.hostBlock(
          server.hostname,
          async () => {
            log.say(`├── ID: ${server.id}`, 2);
            log.say(`├── Subnet: ${server.subnet}`, 2);
            log.say(`├── WireGuard IP: ${server.wireguardIp}`, 2);
            log.say(`├── Management IP: ${server.managementIp}`, 2);

            // Check WireGuard status
            const wgStatus = await getWireGuardStatus(ssh);
            if (wgStatus.up) {
              log.say(
                `├── WireGuard: UP (${wgStatus.peers} peers)`,
                2,
              );
            } else if (wgStatus.exists) {
              log.say(
                `├── WireGuard: DOWN (not active)`,
                2,
              );
            } else {
              log.say(`├── WireGuard: NOT CONFIGURED`, 2);
            }

            // Check Corrosion status
            if (topology.discovery === "corrosion") {
              const corrRunning = await isCorrosionRunning(ssh);
              log.say(
                `├── Corrosion: ${corrRunning ? "RUNNING" : "NOT RUNNING"}`,
                2,
              );
            }

            // Check DNS status
            const dnsRunning = await isJijiDnsRunning(ssh);
            log.say(`├── DNS: ${dnsRunning ? "RUNNING" : "NOT RUNNING"}`, 2);

            // Check UFW status and forward rules
            const ufwActive = await isUfwActive(ssh);
            if (ufwActive) {
              const serverIndex = parseInt(server.subnet.split(".")[2]);
              const containerThirdOctet = 128 + serverIndex;
              const baseNetwork = server.subnet.split(".").slice(0, 2)
                .join(".");
              const containerSubnet =
                `${baseNetwork}.${containerThirdOctet}.0/24`;
              const hasRules = await hasContainerForwardRules(
                ssh,
                containerSubnet,
              );
              log.say(
                `├── UFW: ACTIVE (forward rules ${
                  hasRules ? "configured" : "missing!"
                })`,
                2,
              );
            } else {
              log.say(`├── UFW: INACTIVE`, 2);
            }

            // Query containers on this server (if Corrosion is running)
            if (topology.discovery === "corrosion") {
              try {
                const services = config.getServiceNames();
                const containers: Array<
                  {
                    service: string;
                    ip: string;
                    baseDomain: string;
                    instanceDomain?: string;
                  }
                > = [];

                for (const serviceName of services) {
                  const containerDetails =
                    await queryServerServiceContainerDetails(
                      ssh,
                      serviceName,
                      server.id,
                    );
                  for (const detail of containerDetails) {
                    const baseDomain =
                      `${config.project}-${serviceName}.${topology.serviceDomain}`;
                    const instanceDomain = detail.instanceId
                      ? `${config.project}-${serviceName}-${detail.instanceId}.${topology.serviceDomain}`
                      : undefined;
                    containers.push({
                      service: serviceName,
                      ip: detail.ip,
                      baseDomain,
                      instanceDomain,
                    });
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
                  log.say(`└── Containers:`, 2);
                  for (let i = 0; i < containers.length; i++) {
                    const container = containers[i];
                    const isLast = i === containers.length - 1;
                    const prefix = isLast ? "└──" : "├──";

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

                    // Display both base domain and instance-specific domain if it exists
                    const domainInfo = container.instanceDomain
                      ? `${container.baseDomain}, ${container.instanceDomain}`
                      : container.baseDomain;

                    log.say(
                      `    ${prefix} ${container.service}: ${container.ip} (${domainInfo})${proxyInfo}`,
                      2,
                    );
                  }
                } else {
                  log.say(`└── Containers: none`, 2);
                }
              } catch (error) {
                log.say(`└── Containers: failed to query (${error})`, 2);
              }
            }
          },
          { indent: 1 },
        );
      }

      log.success("\nNetwork status check complete", 0);
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
