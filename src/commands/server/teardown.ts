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
import type { GlobalOptions } from "../../types.ts";

export const teardownCommand = new Command()
  .description("Complete server teardown (containers, engine, network)")
  .option("-y, --confirmed", "Skip confirmation prompt", { default: false })
  .action(async (options) => {
    const globalOptions = options as unknown as GlobalOptions;
    let ctx: Awaited<ReturnType<typeof setupCommandContext>> | undefined;

    try {
      log.section("Server Teardown");

      // Set up command context
      ctx = await setupCommandContext(globalOptions);
      const { config, sshManagers, targetHosts } = ctx;

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

      log.say("Starting server teardown process...");

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
      const removeTracker = log.createStepTracker("Removing Services");

      const services = Array.from(config.services.values());

      for (const service of services) {
        removeTracker.step(`Removing service: ${service.name}`);

        for (const server of service.servers) {
          const host = server.host;
          if (!targetHosts.includes(host)) {
            continue;
          }

          const hostSsh = sshManagers.find((ssh) => ssh.getHost() === host);
          if (!hostSsh) continue;

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
                removeTracker.remote(
                  host,
                  `Removed ${service.name} from proxy`,
                  { indent: 1 },
                );
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
                removeTracker.remote(
                  host,
                  `Unregistered ${service.name} from network`,
                  { indent: 1 },
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
            removeTracker.remote(
              host,
              `Removed container ${containerName}`,
              { indent: 1 },
            );

            // Remove named volumes
            const namedVolumes = service.getNamedVolumes();
            for (const volumeName of namedVolumes) {
              await executeBestEffort(
                hostSsh,
                `${config.builder.engine} volume rm ${volumeName}`,
                `removing volume ${volumeName}`,
              );
              removeTracker.remote(
                host,
                `Removed volume ${volumeName}`,
                { indent: 1 },
              );
            }
          } catch (error) {
            removeTracker.remote(
              host,
              `Failed to remove ${service.name}: ${error}`,
              { indent: 1 },
            );
          }
        }
      }

      removeTracker.finish();

      // Purge container engine system
      const purgeTracker = log.createStepTracker(
        "Purging Container Engine System",
      );

      for (const host of targetHosts) {
        const hostSsh = sshManagers.find((ssh) => ssh.getHost() === host);
        if (!hostSsh) continue;

        try {
          purgeTracker.remote(host, "Stopping all containers...");
          await executeBestEffort(
            hostSsh,
            `${config.builder.engine} stop $(${config.builder.engine} ps -aq)`,
            "stopping containers",
          );

          purgeTracker.remote(host, "Removing all containers...");
          await executeBestEffort(
            hostSsh,
            `${config.builder.engine} rm -f $(${config.builder.engine} ps -aq)`,
            "removing containers",
          );

          purgeTracker.remote(host, "Removing all images...");
          await executeBestEffort(
            hostSsh,
            `${config.builder.engine} rmi -f $(${config.builder.engine} images -aq)`,
            "removing images",
          );

          purgeTracker.remote(host, "Removing all volumes...");
          await executeBestEffort(
            hostSsh,
            `${config.builder.engine} volume rm $(${config.builder.engine} volume ls -q)`,
            "removing volumes",
          );

          purgeTracker.remote(host, "Removing all networks...");
          await executeBestEffort(
            hostSsh,
            `${config.builder.engine} network prune -f`,
            "pruning networks",
          );

          purgeTracker.remote(host, "Running system prune...");
          await executeBestEffort(
            hostSsh,
            `${config.builder.engine} system prune -a -f --volumes`,
            "system prune",
          );

          purgeTracker.remote(host, "Container engine system purged");
        } catch (error) {
          purgeTracker.remote(host, `System purge failed: ${error}`);
        }
      }

      purgeTracker.finish();

      // Remove container engine
      const engineTracker = log.createStepTracker(
        "Removing Container Engine",
      );

      for (const host of targetHosts) {
        const hostSsh = sshManagers.find((ssh) => ssh.getHost() === host);
        if (!hostSsh) continue;

        try {
          const engine = config.builder.engine;

          if (engine === "docker") {
            engineTracker.remote(host, "Removing Docker packages...");
            await executeBestEffort(
              hostSsh,
              "systemctl stop docker.socket docker.service",
              "stopping Docker service",
            );
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

            engineTracker.remote(host, "Removing Docker files...");
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
            engineTracker.remote(host, "Removing Podman packages...");
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

            engineTracker.remote(host, "Removing Podman files...");
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

          engineTracker.remote(host, `${engine} removed`);
        } catch (error) {
          engineTracker.remote(host, `Engine removal failed: ${error}`);
        }
      }

      engineTracker.finish();

      // Tear down network
      if (config.network.enabled) {
        const networkTracker = log.createStepTracker("Tearing Down Network");

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
          networkTracker.step("No network cluster found, skipping");
        } else {
          networkTracker.step(
            `Tearing down network on ${topology.servers.length} server(s)`,
          );

          for (const server of topology.servers) {
            const ssh = sshManagers.find((s) =>
              s.getHost() === server.hostname
            );

            if (!ssh) {
              continue;
            }

            try {
              networkTracker.remote(server.hostname, "Stopping DNS service...");
              await stopCoreDNSService(ssh);

              if (topology.discovery === "corrosion") {
                networkTracker.remote(
                  server.hostname,
                  "Stopping Corrosion service...",
                );
                await stopCorrosionService(ssh);
              }

              networkTracker.remote(
                server.hostname,
                "Stopping WireGuard interface...",
              );
              await bringDownWireGuardInterface(ssh);
              await disableWireGuardService(ssh);

              networkTracker.remote(
                server.hostname,
                "Removing network configuration files...",
              );
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

              networkTracker.remote(
                server.hostname,
                "Network teardown complete",
              );
            } catch (error) {
              networkTracker.remote(
                server.hostname,
                `Network teardown failed: ${error}`,
              );
            }
          }
        }

        networkTracker.finish();
      }

      // Remove project directory
      const cleanupTracker = log.createStepTracker(
        "Removing Project Directory",
      );

      const projectDir = `.jiji/${config.project}`;

      for (const host of targetHosts) {
        const hostSsh = sshManagers.find((ssh) => ssh.getHost() === host);
        if (!hostSsh) continue;

        try {
          await hostSsh.executeCommand(`rm -rf ${projectDir}`);
          cleanupTracker.remote(host, `Removed ${projectDir}`);
        } catch (error) {
          cleanupTracker.remote(
            host,
            `Failed to remove ${projectDir}: ${error}`,
          );
        }
      }

      cleanupTracker.finish();

      // Log teardown completion
      await auditLogger.logCustomCommand(
        "server_teardown",
        "success",
        `Server teardown completed successfully on ${targetHosts.length} host(s)`,
      );

      console.log();
      log.say("Server teardown completed successfully");
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
