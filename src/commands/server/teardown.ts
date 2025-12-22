/**
 * Server teardown command
 *
 * Performs complete teardown of server infrastructure including:
 * - Stopping and removing all containers
 * - Purging the container engine system
 * - Removing the container engine
 * - Reverting all network changes
 */

import { Command } from "@cliffy/command";
import { Confirm } from "@cliffy/prompt";
import {
  cleanupSSHConnections,
  setupCommandContext,
} from "../../utils/command_helpers.ts";
import { handleCommandError } from "../../utils/error_handler.ts";
import { log, Logger } from "../../utils/logger.ts";
import { createServerAuditLogger } from "../../utils/audit.ts";
import { unregisterContainerFromNetwork } from "../../lib/services/container_registry.ts";
import { loadTopology } from "../../lib/network/topology.ts";
import {
  bringDownWireGuardInterface,
  disableWireGuardService,
} from "../../lib/network/wireguard.ts";
import { stopCorrosionService } from "../../lib/network/corrosion.ts";
import { stopCoreDNSService } from "../../lib/network/dns.ts";
import { ProxyCommands } from "../../utils/proxy.ts";
import { DEFAULT_MAX_PREFIX_LENGTH } from "../../constants.ts";
import type { GlobalOptions } from "../../types.ts";
import type { ServiceConfiguration } from "../../lib/configuration/service.ts";

/**
 * Extracts named volumes from a service's volume configuration.
 * Named volumes are those that don't start with "/" or "./" (not host paths).
 */
function getNamedVolumes(service: ServiceConfiguration): string[] {
  const namedVolumes: string[] = [];

  for (const volume of service.volumes) {
    const parts = volume.split(":");
    if (parts.length >= 2) {
      const source = parts[0];
      if (!source.startsWith("/") && !source.startsWith("./")) {
        namedVolumes.push(source);
      }
    }
  }

  return namedVolumes;
}

export const teardownCommand = new Command()
  .description("Complete server teardown (containers, engine, network)")
  .option("-y, --confirmed", "Skip confirmation prompt", { default: false })
  .action(async (options) => {
    const globalOptions = options as unknown as GlobalOptions;
    let ctx: Awaited<ReturnType<typeof setupCommandContext>> | undefined;

    try {
      await log.group("Server Teardown", async () => {
        // Set up command context
        ctx = await setupCommandContext(globalOptions);
        const { config, sshManagers, targetHosts } = ctx;

        // Get confirmation unless --confirmed flag is passed
        const confirmed = options.confirmed as boolean;
        if (!confirmed) {
          console.log();
          log.warn(
            "DESTRUCTIVE OPERATION - This will:",
            "teardown",
          );
          log.warn(
            `  Stop and remove ALL containers on ${targetHosts.length} server(s)`,
            "teardown",
          );
          log.warn(
            `  Purge the ${config.builder.engine} system (all images, volumes, networks)`,
            "teardown",
          );
          log.warn(
            `  Remove the ${config.builder.engine} container engine`,
            "teardown",
          );
          if (config.network.enabled) {
            log.warn(
              "  Tear down the private network (WireGuard, Corrosion, CoreDNS)",
              "teardown",
            );
          }
          log.warn(
            `  Remove .jiji/${config.project} directory`,
            "teardown",
          );
          console.log();

          const confirm = await Confirm.prompt({
            message: "Are you absolutely sure you want to proceed?",
            default: false,
          });

          if (!confirm) {
            log.info("Teardown cancelled by user", "teardown");
            return;
          }
        }

        log.info("Starting server teardown process...", "teardown");

        // Create audit logger
        const auditLogger = createServerAuditLogger(
          sshManagers,
          config.project,
        );

        // Log teardown start
        await auditLogger.logCustomCommand(
          "server_teardown",
          "started",
          `Starting complete server teardown on ${targetHosts.length} host(s)`,
        );

        // Create server loggers for individual host reporting
        const serverLoggers = Logger.forServers(targetHosts, {
          maxPrefixLength: DEFAULT_MAX_PREFIX_LENGTH,
        });

        // Remove all services and containers
        await log.group("Removing Services", async () => {
          const services = Array.from(config.services.values());

          for (const service of services) {
            log.status(`Removing service: ${service.name}`, "teardown");

            for (const server of service.servers) {
              const host = server.host;
              if (!targetHosts.includes(host)) {
                log.warn(
                  `Skipping ${service.name} on unreachable host: ${host}`,
                  "teardown",
                );
                continue;
              }

              const hostSsh = sshManagers.find((ssh) => ssh.getHost() === host);
              if (!hostSsh) continue;

              const serverLogger = serverLoggers.get(host)!;

              try {
                const containerName = service.getContainerName();

                // Remove service from proxy if proxy is configured
                if (service.proxy?.enabled) {
                  try {
                    const proxyCmd = new ProxyCommands(
                      config.builder.engine,
                      hostSsh,
                    );
                    await proxyCmd.remove(service.name);
                    serverLogger.info(`Removed ${service.name} from proxy`);
                  } catch (error) {
                    serverLogger.warn(
                      `Failed to remove from proxy: ${error}`,
                    );
                  }
                }

                // Unregister from network (if enabled)
                if (config.network.enabled) {
                  try {
                    await unregisterContainerFromNetwork(
                      hostSsh,
                      containerName,
                      service.name,
                      config.project,
                    );
                    serverLogger.info(
                      `Unregistered ${service.name} from network`,
                    );
                  } catch (error) {
                    serverLogger.warn(
                      `Failed to unregister from network: ${error}`,
                    );
                  }
                }

                // Stop and remove the container
                await hostSsh.executeCommand(
                  `${config.builder.engine} rm -f ${containerName} 2>/dev/null || true`,
                );
                serverLogger.success(`Removed container ${containerName}`);

                // Remove named volumes
                const namedVolumes = getNamedVolumes(service);
                for (const volumeName of namedVolumes) {
                  try {
                    await hostSsh.executeCommand(
                      `${config.builder.engine} volume rm ${volumeName} 2>/dev/null || true`,
                    );
                    serverLogger.info(`Removed volume ${volumeName}`);
                  } catch (error) {
                    serverLogger.warn(
                      `Failed to remove volume ${volumeName}: ${error}`,
                    );
                  }
                }
              } catch (error) {
                serverLogger.error(
                  `Failed to remove ${service.name}: ${error}`,
                );
              }
            }
          }

          log.success("All services removed", "teardown");
        });

        // Purge container engine system
        await log.group(
          "Purging Container Engine System",
          async () => {
            for (const host of targetHosts) {
              const hostSsh = sshManagers.find((ssh) => ssh.getHost() === host);
              if (!hostSsh) continue;

              const serverLogger = serverLoggers.get(host)!;

              try {
                serverLogger.info("Stopping all containers...");
                await hostSsh.executeCommand(
                  `${config.builder.engine} stop $(${config.builder.engine} ps -aq) 2>/dev/null || true`,
                );

                serverLogger.info("Removing all containers...");
                await hostSsh.executeCommand(
                  `${config.builder.engine} rm -f $(${config.builder.engine} ps -aq) 2>/dev/null || true`,
                );

                serverLogger.info("Removing all images...");
                await hostSsh.executeCommand(
                  `${config.builder.engine} rmi -f $(${config.builder.engine} images -aq) 2>/dev/null || true`,
                );

                serverLogger.info("Removing all volumes...");
                await hostSsh.executeCommand(
                  `${config.builder.engine} volume rm $(${config.builder.engine} volume ls -q) 2>/dev/null || true`,
                );

                serverLogger.info("Removing all networks...");
                await hostSsh.executeCommand(
                  `${config.builder.engine} network prune -f 2>/dev/null || true`,
                );

                serverLogger.info("Running system prune...");
                await hostSsh.executeCommand(
                  `${config.builder.engine} system prune -a -f --volumes 2>/dev/null || true`,
                );

                serverLogger.success("Container engine system purged");
              } catch (error) {
                serverLogger.error(`System purge failed: ${error}`);
              }
            }

            log.success(
              "Container engine system purged on all hosts",
              "teardown",
            );
          },
        );

        // Remove container engine
        await log.group("Removing Container Engine", async () => {
          for (const host of targetHosts) {
            const hostSsh = sshManagers.find((ssh) => ssh.getHost() === host);
            if (!hostSsh) continue;

            const serverLogger = serverLoggers.get(host)!;

            try {
              const engine = config.builder.engine;

              if (engine === "docker") {
                serverLogger.info("Stopping Docker service...");
                await hostSsh.executeCommand(
                  "systemctl stop docker.socket docker.service 2>/dev/null || true",
                );

                serverLogger.info("Removing Docker packages...");
                await hostSsh.executeCommand(
                  "apt-get remove -y docker.io docker-compose 2>/dev/null || true",
                );
                await hostSsh.executeCommand(
                  "apt-get autoremove -y 2>/dev/null || true",
                );

                serverLogger.info("Removing Docker files...");
                await hostSsh.executeCommand("rm -rf /var/lib/docker");
                await hostSsh.executeCommand("rm -rf /etc/docker");
              } else if (engine === "podman") {
                serverLogger.info("Removing Podman packages...");
                await hostSsh.executeCommand(
                  "apt-get remove -y podman 2>/dev/null || true",
                );
                await hostSsh.executeCommand(
                  "apt-get autoremove -y 2>/dev/null || true",
                );

                serverLogger.info("Removing Podman files...");
                await hostSsh.executeCommand("rm -rf /var/lib/containers");
                await hostSsh.executeCommand("rm -rf /etc/containers");
              }

              serverLogger.success(`${engine} removed`);
            } catch (error) {
              serverLogger.error(`Engine removal failed: ${error}`);
            }
          }

          log.success("Container engine removed from all hosts", "teardown");
        });

        // Tear down network
        if (config.network.enabled) {
          await log.group("Tearing Down Network", async () => {
            // Try to load topology
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
              log.info(
                "No network cluster found, skipping network teardown",
                "network",
              );
            } else {
              log.info(
                `Tearing down network on ${topology.servers.length} server(s)`,
                "network",
              );

              for (const server of topology.servers) {
                const ssh = sshManagers.find((s) =>
                  s.getHost() === server.hostname
                );
                const serverLogger = serverLoggers.get(server.hostname)!;

                if (!ssh) {
                  serverLogger.warn("SSH connection not available, skipping");
                  continue;
                }

                try {
                  serverLogger.info("Stopping DNS service...");
                  await stopCoreDNSService(ssh);

                  if (topology.discovery === "corrosion") {
                    serverLogger.info("Stopping Corrosion service...");
                    await stopCorrosionService(ssh);
                  }

                  serverLogger.info("Stopping WireGuard interface...");
                  await bringDownWireGuardInterface(ssh);
                  await disableWireGuardService(ssh);

                  serverLogger.info("Removing network configuration files...");
                  await ssh.executeCommand("rm -f /etc/wireguard/jiji0.conf");
                  await ssh.executeCommand("rm -rf /opt/jiji/corrosion");
                  await ssh.executeCommand("rm -rf /opt/jiji/dns");
                  await ssh.executeCommand(
                    "rm -f /etc/systemd/system/jiji-corrosion.service",
                  );
                  await ssh.executeCommand(
                    "rm -f /etc/systemd/system/jiji-dns.service",
                  );
                  await ssh.executeCommand(
                    "rm -f /etc/systemd/system/jiji-dns-update.service",
                  );
                  await ssh.executeCommand(
                    "rm -f /etc/systemd/system/jiji-dns-update.timer",
                  );
                  await ssh.executeCommand(
                    "rm -f /etc/systemd/system/jiji-control-loop.service",
                  );
                  await ssh.executeCommand("systemctl daemon-reload");

                  serverLogger.success("Network teardown complete");
                } catch (error) {
                  serverLogger.error(`Network teardown failed: ${error}`);
                }
              }

              log.success("Network teardown complete on all hosts", "network");
            }
          });
        }

        // Remove project directory
        await log.group("Removing Project Directory", async () => {
          const projectDir = `.jiji/${config.project}`;

          for (const host of targetHosts) {
            const hostSsh = sshManagers.find((ssh) => ssh.getHost() === host);
            if (!hostSsh) continue;

            const serverLogger = serverLoggers.get(host)!;

            try {
              await hostSsh.executeCommand(`rm -rf ${projectDir}`);
              serverLogger.success(`Removed ${projectDir}`);
            } catch (error) {
              serverLogger.error(`Failed to remove ${projectDir}: ${error}`);
            }
          }

          log.success(`Project directory removed from all hosts`, "teardown");
        });

        // Log teardown completion
        await auditLogger.logCustomCommand(
          "server_teardown",
          "success",
          `Server teardown completed successfully on ${targetHosts.length} host(s)`,
        );

        log.success("Server teardown completed successfully", "teardown");
      });
    } catch (error) {
      await handleCommandError(error, {
        operation: "Server teardown",
        component: "teardown",
        sshManagers: ctx?.sshManagers,
        projectName: ctx?.config?.project,
        targetHosts: ctx?.targetHosts,
        customAuditLogger: async (errorMessage) => {
          if (ctx?.sshManagers && ctx?.config) {
            const auditLogger = createServerAuditLogger(
              ctx.sshManagers,
              ctx.config.project,
            );
            await auditLogger.logCustomCommand(
              "server_teardown",
              "failed",
              errorMessage,
            );
          }
        },
      });
    } finally {
      if (ctx?.sshManagers) {
        cleanupSSHConnections(ctx.sshManagers);
      }
    }
  });
