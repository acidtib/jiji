import { Command } from "@cliffy/command";
import { Confirm } from "@cliffy/prompt";
import {
  cleanupSSHConnections,
  executeBestEffort,
  findSSHManagerByHost,
  setupCommandContext,
} from "../utils/command_helpers.ts";
import { handleCommandError } from "../utils/error_handler.ts";
import { getTreePrefix, log } from "../utils/logger.ts";
import { ProxyCommands } from "../utils/proxy.ts";
import { unregisterContainerFromNetwork } from "../lib/services/container_registry.ts";
import type { GlobalOptions } from "../types.ts";

export const removeCommand = new Command()
  .description("Remove services and .jiji/project_dir from servers")
  .option("-y, --confirmed", "Skip confirmation prompt", { default: false })
  .action(async (options) => {
    const globalOptions = options as unknown as GlobalOptions;
    let ctx: Awaited<ReturnType<typeof setupCommandContext>> | undefined;

    try {
      log.section("Service Removal:");

      // Load configuration first (before confirmation)
      const { Configuration } = await import("../lib/configuration.ts");
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

      // Get confirmation unless --confirmed flag is passed
      const confirmed = options.confirmed as boolean;
      if (!confirmed) {
        console.log();
        log.warn(
          `This will remove all services and .jiji/${config.project} from ${allHosts.length} server(s)`,
        );
        console.log();

        const confirm = await Confirm.prompt({
          message: "Are you sure you want to proceed?",
          default: false,
        });

        if (!confirm) {
          log.say("Removal cancelled by user");
          return;
        }
      }

      // Set up command context (establish SSH connections)
      ctx = await setupCommandContext(globalOptions);
      const { config: ctxConfig, sshManagers, targetHosts } = ctx;

      // Display SSH connection status
      console.log("");
      for (const ssh of sshManagers) {
        log.remote(ssh.getHost(), ": Connected", { indent: 1 });
      }

      // Get all services
      const services = Array.from(ctxConfig.services.values());

      // Remove containers and proxy configuration for each service
      log.section("Removing Services:");

      for (const service of services) {
        log.say(`- Removing ${service.name}`, 1);

        for (const server of service.servers) {
          const host = server.host;
          if (!targetHosts.includes(host)) {
            continue;
          }

          const hostSsh = findSSHManagerByHost(sshManagers, host);
          if (!hostSsh) continue;

          await log.hostBlock(host, async () => {
            try {
              const containerName = service.getContainerName();
              const namedVolumes = service.getNamedVolumes();
              const hasNamedVolumes = namedVolumes.length > 0;
              const isLastItem = !hasNamedVolumes; // If no named volumes, this is the last item

              // Remove service from proxy if proxy is configured
              if (service.proxy?.enabled) {
                try {
                  const proxyCmd = new ProxyCommands(
                    ctxConfig.builder.engine,
                    hostSsh,
                  );
                  await proxyCmd.remove(service.name);
                  log.say(`├── Removed ${service.name} from proxy`, 2);
                } catch (_error) {
                  // Best effort removal
                }
              }

              // Unregister from network (if enabled)
              if (ctxConfig.network.enabled) {
                try {
                  await unregisterContainerFromNetwork(
                    hostSsh,
                    containerName,
                    service.name,
                    ctxConfig.project,
                  );
                  log.say(`├── Unregistered ${service.name} from network`, 2);
                } catch (_error) {
                  // Best effort removal
                }
              }

              // Stop and remove the container
              await executeBestEffort(
                hostSsh,
                `${ctxConfig.builder.engine} rm -f ${containerName}`,
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
                const prefix = getTreePrefix(i, namedVolumes.length);

                await executeBestEffort(
                  hostSsh,
                  `${ctxConfig.builder.engine} volume rm ${volumeName}`,
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

      // Remove .jiji/project directory from all hosts
      log.section("Removing Project Directory:");

      const projectDir = `.jiji/${ctxConfig.project}`;

      for (let i = 0; i < targetHosts.length; i++) {
        const host = targetHosts[i];
        const hostSsh = findSSHManagerByHost(sshManagers, host);
        if (!hostSsh) continue;

        const prefix = getTreePrefix(i, targetHosts.length);
        await log.hostBlock(host, async () => {
          try {
            await hostSsh.executeCommand(`rm -rf ${projectDir}`);
            log.say(`${prefix} Removed ${projectDir}`, 2);
          } catch (error) {
            log.say(
              `${prefix} Failed to remove ${projectDir}: ${error}`,
              2,
            );
          }
        }, { indent: 1 });
      }

      log.success("\nRemoval completed successfully", 0);
    } catch (error) {
      await handleCommandError(error, {
        operation: "Removal",
        component: "remove",
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
