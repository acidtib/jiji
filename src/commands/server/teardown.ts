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
  executeBestEffort,
  setupCommandContext,
} from "../../utils/command_helpers.ts";
import { handleCommandError } from "../../utils/error_handler.ts";
import { log } from "../../utils/logger.ts";
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
import {
  Configuration,
  ServiceConfiguration,
} from "../../lib/configuration.ts";
import type { GlobalOptions } from "../../types.ts";

export const teardownCommand = new Command()
  .description("Complete server teardown (containers, engine, network)")
  .option("-y, --confirmed", "Skip confirmation prompt", { default: false })
  .action(async (options) => {
    const globalOptions = options as unknown as GlobalOptions;
    let ctx: Awaited<ReturnType<typeof setupCommandContext>> | undefined;

    try {
      log.section("Server Teardown:");

      // Load configuration first (before SSH setup)
      const config = await Configuration.load(
        globalOptions.environment,
        globalOptions.configFile,
      );

      const configPath = config.configPath || "unknown";
      const allHosts = config.getAllServerHosts();

      log.say(`Configuration loaded from: ${configPath}`, 1);
      log.say(`Container engine: ${config.builder.engine}`, 1);
      log.say(
        `Found ${allHosts.length} remote host(s): ${allHosts.join(", ")}`,
        1,
      );

      // Now setup SSH connections
      ctx = await setupCommandContext(globalOptions);
      const { sshManagers, targetHosts } = ctx;

      // Show connection status for each host
      console.log(""); // Empty line
      for (const ssh of sshManagers) {
        log.remote(ssh.getHost(), ": Connected", { indent: 1 });
      }

      // Get confirmation unless --confirmed flag is passed
      const confirmed = options.confirmed as boolean;
      if (!confirmed) {
        console.log();
        log.warn("DESTRUCTIVE OPERATION - This will:");
        log.say(
          `Stop and remove ALL containers on ${targetHosts.length} server(s)`,
          1,
        );
        log.say(
          `Purge the ${config.builder.engine} system (all images, volumes, networks)`,
          1,
        );
        log.say(
          `Remove the ${config.builder.engine} container engine`,
          1,
        );
        if (config.network.enabled) {
          log.say(
            "Tear down the private network (WireGuard, Corrosion, CoreDNS)",
            1,
          );
        }
        log.say(
          `Remove .jiji/${config.project} directory`,
          1,
        );
        console.log();

        const confirm = await Confirm.prompt({
          message: "Are you absolutely sure you want to proceed?",
          default: false,
        });

        if (!confirm) {
          log.say("Teardown cancelled by user");
          return;
        }
      }

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

      // Remove all services and containers
      log.section("Removing Services:");

      const services: ServiceConfiguration[] = Array.from(
        config.services.values(),
      );

      for (const service of services) {
        log.say(`- Removing ${service.name}`, 1);

        for (const server of service.servers) {
          const host = server.host;
          if (!targetHosts.includes(host)) {
            continue;
          }

          const hostSsh = sshManagers.find((ssh) => ssh.getHost() === host);
          if (!hostSsh) continue;

          await log.hostBlock(host, async () => {
            try {
              const containerName = service.getContainerName();
              const namedVolumes = service.getNamedVolumes();
              const isLastItem = namedVolumes.length === 0;

              // Remove service from proxy if proxy is configured
              if (service.proxy?.enabled) {
                try {
                  const proxyCmd = new ProxyCommands(
                    config.builder.engine,
                    hostSsh,
                  );
                  await proxyCmd.remove(service.name);
                  log.say(`├── Removed ${service.name} from proxy`, 2);
                } catch (_error) {
                  // Best effort
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
                  log.say(
                    `├── Unregistered ${containerName} from network`,
                    2,
                  );
                } catch (_error) {
                  // Best effort
                }
              }

              // Stop and remove the container
              await executeBestEffort(
                hostSsh,
                `${config.builder.engine} rm -f ${containerName}`,
                `removing container ${containerName}`,
              );
              const containerPrefix = isLastItem ? "└──" : "├──";
              log.say(
                `${containerPrefix} Removed container ${containerName}`,
                2,
              );

              // Remove named volumes
              for (let i = 0; i < namedVolumes.length; i++) {
                const volumeName = namedVolumes[i];
                const isLast = i === namedVolumes.length - 1;
                const prefix = isLast ? "└──" : "├──";

                await executeBestEffort(
                  hostSsh,
                  `${config.builder.engine} volume rm ${volumeName}`,
                  `removing volume ${volumeName}`,
                );
                log.say(`${prefix} Removed volume ${volumeName}`, 2);
              }
            } catch (error) {
              log.say(`└── Failed to remove ${service.name}: ${error}`, 2);
            }
          }, { indent: 1 });
        }
      }

      // Purge container engine system
      log.section("Purging Container Engine System:");

      for (const host of targetHosts) {
        const hostSsh = sshManagers.find((ssh) => ssh.getHost() === host);
        if (!hostSsh) continue;

        await log.hostBlock(host, async () => {
          try {
            log.say("├── Stopping all containers", 2);
            await executeBestEffort(
              hostSsh,
              `${config.builder.engine} stop $(${config.builder.engine} ps -aq)`,
              "stopping containers",
            );

            log.say("├── Removing all containers", 2);
            await executeBestEffort(
              hostSsh,
              `${config.builder.engine} rm -f $(${config.builder.engine} ps -aq)`,
              "removing containers",
            );

            log.say("├── Removing all images", 2);
            await executeBestEffort(
              hostSsh,
              `${config.builder.engine} rmi -f $(${config.builder.engine} images -aq)`,
              "removing images",
            );

            log.say("├── Removing all volumes", 2);
            await executeBestEffort(
              hostSsh,
              `${config.builder.engine} volume rm $(${config.builder.engine} volume ls -q)`,
              "removing volumes",
            );

            log.say("├── Removing all networks", 2);
            await executeBestEffort(
              hostSsh,
              `${config.builder.engine} network prune -f`,
              "pruning networks",
            );

            log.say("└── Running system prune", 2);
            await executeBestEffort(
              hostSsh,
              `${config.builder.engine} system prune -a -f --volumes`,
              "system prune",
            );
          } catch (error) {
            log.say(`└── System purge failed: ${error}`, 2);
          }
        }, { indent: 1 });
      }

      // Remove container engine
      log.section("Removing Container Engine:");

      for (const host of targetHosts) {
        const hostSsh = sshManagers.find((ssh) => ssh.getHost() === host);
        if (!hostSsh) continue;

        await log.hostBlock(host, async () => {
          try {
            const engine = config.builder.engine;

            if (engine === "docker") {
              log.say("├── Stopping Docker service", 2);
              await executeBestEffort(
                hostSsh,
                "systemctl stop docker.socket docker.service",
                "stopping Docker service",
              );

              log.say("├── Removing Docker packages", 2);
              await executeBestEffort(
                hostSsh,
                "apt-get remove -y docker.io docker-compose",
                "removing Docker packages",
              );
              await executeBestEffort(
                hostSsh,
                "apt-get autoremove -y",
                "autoremove packages",
              );

              log.say("├── Removing Docker files", 2);
              await executeBestEffort(
                hostSsh,
                "rm -rf /var/lib/docker",
                "removing Docker data",
              );
              await executeBestEffort(
                hostSsh,
                "rm -rf /etc/docker",
                "removing Docker config",
              );
            } else if (engine === "podman") {
              log.say("├── Removing Podman packages", 2);
              await executeBestEffort(
                hostSsh,
                "apt-get remove -y podman",
                "removing Podman packages",
              );
              await executeBestEffort(
                hostSsh,
                "apt-get autoremove -y",
                "autoremove packages",
              );

              log.say("├── Removing Podman files", 2);
              await executeBestEffort(
                hostSsh,
                "rm -rf /var/lib/containers",
                "removing Podman data",
              );
              await executeBestEffort(
                hostSsh,
                "rm -rf /etc/containers",
                "removing Podman config",
              );
            }

            log.say(`└── ${engine} removed`, 2);
          } catch (error) {
            log.say(`└── Engine removal failed: ${error}`, 2);
          }
        }, { indent: 1 });
      }

      // Tear down network
      if (config.network.enabled) {
        log.section("Tearing Down Network:");

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
          log.say("- No network cluster found, skipping", 1);
        } else {
          log.say(
            `- Tearing down network on ${topology.servers.length} server(s)`,
            1,
          );

          for (const server of topology.servers) {
            const ssh = sshManagers.find((s) =>
              s.getHost() === server.hostname
            );

            if (!ssh) {
              continue;
            }

            await log.hostBlock(server.hostname, async () => {
              try {
                log.say("├── Stopping DNS service", 2);
                await stopCoreDNSService(ssh);

                if (topology.discovery === "corrosion") {
                  log.say("├── Stopping Corrosion service", 2);
                  await stopCorrosionService(ssh);
                }

                log.say("├── Stopping WireGuard interface", 2);
                await bringDownWireGuardInterface(ssh);
                await disableWireGuardService(ssh);

                log.say("├── Removing network configuration files", 2);
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

                log.say("└── Network teardown complete", 2);
              } catch (error) {
                log.say(`└── Network teardown failed: ${error}`, 2);
              }
            }, { indent: 1 });
          }
        }
      }

      // Remove project directory
      log.section("Removing Project Directory:");

      const projectDir = `.jiji/${config.project}`;

      for (const host of targetHosts) {
        const hostSsh = sshManagers.find((ssh) => ssh.getHost() === host);
        if (!hostSsh) continue;

        await log.hostBlock(host, async () => {
          try {
            await hostSsh.executeCommand(`rm -rf ${projectDir}`);
            log.say(`└── Removed ${projectDir}`, 2);
          } catch (error) {
            log.say(`└── Failed to remove ${projectDir}: ${error}`, 2);
          }
        }, { indent: 1 });
      }

      // Log teardown completion
      await auditLogger.logCustomCommand(
        "server_teardown",
        "success",
        `Server teardown completed successfully on ${targetHosts.length} host(s)`,
      );

      log.success(
        `\nTeardown completed successfully on ${targetHosts.length} server(s)`,
        0,
      );
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
