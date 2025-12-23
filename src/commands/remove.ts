import { Command } from "@cliffy/command";
import { Confirm } from "@cliffy/prompt";
import {
  cleanupSSHConnections,
  executeBestEffort,
  setupCommandContext,
} from "../utils/command_helpers.ts";
import { handleCommandError } from "../utils/error_handler.ts";
import { log } from "../utils/logger.ts";
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
      log.say(`Configuration loaded from: ${configPath}`, 1);
      log.say(`Project: ${config.project}`, 1);

      // Collect all unique hosts
      const allHosts = config.getAllServerHosts();

      if (allHosts.length === 0) {
        log.error(
          `No remote hosts found in configuration at: ${configPath}`,
        );
        Deno.exit(1);
      }

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

      // Set up command context
      ctx = await setupCommandContext(globalOptions);
      const { config: ctxConfig, sshManagers, targetHosts } = ctx;

      // Show connection status for each host
      console.log(""); // Empty line
      for (const ssh of sshManagers) {
        log.remote(ssh.getHost(), ": Connected", { indent: 1 });
      }

      // Get all services
      const services = Array.from(ctxConfig.services.values());

      // Remove containers and proxy configuration for each service
      log.section("Removing Services:");

      for (const service of services) {
        log.say(`Service: ${service.name}`, 1);

        for (const server of service.servers) {
          const host = server.host;
          if (!targetHosts.includes(host)) {
            log.warn(
              `Skipping ${service.name} on unreachable host: ${host}`,
            );
            continue;
          }

          const hostSsh = sshManagers.find((ssh) => ssh.getHost() === host);
          if (!hostSsh) continue;

          await log.hostBlock(host, async () => {
            try {
              const containerName = service.getContainerName();

              // Remove service from proxy if proxy is configured
              if (service.proxy?.enabled) {
                try {
                  const proxyCmd = new ProxyCommands(
                    ctxConfig.builder.engine,
                    hostSsh,
                  );
                  await proxyCmd.remove(service.name);
                  log.say(`Removed ${service.name} from proxy`, 3);
                } catch (error) {
                  log.say(
                    `Failed to remove from proxy: ${error}`,
                    3,
                  );
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
                  log.say(`Unregistered ${service.name} from network`, 3);
                } catch (error) {
                  log.say(
                    `Failed to unregister from network: ${error}`,
                    3,
                  );
                }
              }

              // Stop and remove the container
              await executeBestEffort(
                hostSsh,
                `${ctxConfig.builder.engine} rm -f ${containerName}`,
                `removing container ${containerName}`,
              );
              log.say(`Removed container ${containerName}`, 3);

              // Remove named volumes
              const namedVolumes = service.getNamedVolumes();
              if (namedVolumes.length > 0) {
                log.say(`Removing ${namedVolumes.length} named volume(s)`, 3);

                for (const volumeName of namedVolumes) {
                  await executeBestEffort(
                    hostSsh,
                    `${ctxConfig.builder.engine} volume rm ${volumeName}`,
                    `removing volume ${volumeName}`,
                  );
                  log.say(`Removed volume ${volumeName}`, 4);
                }
              }
            } catch (error) {
              log.say(
                `Failed to remove ${service.name}: ${
                  error instanceof Error ? error.message : String(error)
                }`,
                3,
              );
            }
          }, { indent: 2 });
        }
      }

      // Remove .jiji/project directory from all hosts
      log.section("Removing Project Directory:");

      const projectDir = `.jiji/${ctxConfig.project}`;

      for (const host of targetHosts) {
        const hostSsh = sshManagers.find((ssh) => ssh.getHost() === host);
        if (!hostSsh) continue;

        await log.hostBlock(host, async () => {
          try {
            const rmResult = await hostSsh.executeCommand(
              `rm -rf ${projectDir}`,
            );

            if (rmResult.success) {
              log.say(`Removed ${projectDir}`, 2);
            } else {
              log.say(
                `Failed to remove ${projectDir}: ${rmResult.stderr}`,
                2,
              );
            }
          } catch (error) {
            log.say(
              `Error removing ${projectDir}: ${
                error instanceof Error ? error.message : String(error)
              }`,
              2,
            );
          }
        }, { indent: 1 });
      }

      console.log();
      log.say("Removal process completed");
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
