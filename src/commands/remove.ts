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
      const tracker = log.createStepTracker("Service Removal");

      // Load configuration first (before confirmation)
      tracker.step("Loading configuration");
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

      log.section("Service Cleanup");

      // Set up command context
      ctx = await setupCommandContext(globalOptions);
      const { config: ctxConfig, sshManagers, targetHosts } = ctx;

      // Get all services
      const services = Array.from(ctxConfig.services.values());

      // Remove containers and proxy configuration for each service
      const serviceTracker = log.createStepTracker("Removing Services");

      for (const service of services) {
        serviceTracker.step(`Removing service: ${service.name}`);

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

          try {
            const containerName = service.getContainerName();

            // Remove service from proxy if proxy is configured
            if (service.proxy?.enabled) {
              serviceTracker.remote(
                host,
                `Removing ${service.name} from proxy`,
              );
              const proxyCmd = new ProxyCommands(
                ctxConfig.builder.engine,
                hostSsh,
              );
              await proxyCmd.remove(service.name);
              serviceTracker.remote(host, `Removed ${service.name} from proxy`);
            }

            // Unregister from network (if enabled)
            if (ctxConfig.network.enabled) {
              try {
                serviceTracker.remote(
                  host,
                  `Unregistering ${service.name} from network`,
                  { indent: 1 },
                );
                await unregisterContainerFromNetwork(
                  hostSsh,
                  containerName,
                  service.name,
                  ctxConfig.project,
                );
              } catch (error) {
                log.warn(
                  `Failed to unregister from network: ${error}`,
                );
              }
            }

            // Stop and remove the container
            serviceTracker.remote(host, `Removing container ${containerName}`, {
              indent: 1,
            });
            await executeBestEffort(
              hostSsh,
              `${ctxConfig.builder.engine} rm -f ${containerName}`,
              `removing container ${containerName}`,
            );
            serviceTracker.remote(host, `Removed container ${containerName}`, {
              indent: 1,
            });

            const namedVolumes = service.getNamedVolumes();
            if (namedVolumes.length > 0) {
              serviceTracker.remote(
                host,
                `Removing ${namedVolumes.length} named volume(s)`,
                { indent: 1 },
              );

              for (const volumeName of namedVolumes) {
                await executeBestEffort(
                  hostSsh,
                  `${ctxConfig.builder.engine} volume rm ${volumeName}`,
                  `removing volume ${volumeName}`,
                );
                serviceTracker.remote(host, `Removed volume ${volumeName}`, {
                  indent: 2,
                });
              }
            }
          } catch (error) {
            log.error(
              `Failed to remove ${service.name} on ${host}: ${
                error instanceof Error ? error.message : String(error)
              }`,
            );
          }
        }
      }

      serviceTracker.finish();

      // Remove .jiji/project directory from all hosts
      const dirTracker = log.createStepTracker("Removing Project Directory");
      const projectDir = `.jiji/${ctxConfig.project}`;
      dirTracker.step(`Removing ${projectDir} from all hosts`);

      for (const host of targetHosts) {
        const hostSsh = sshManagers.find((ssh) => ssh.getHost() === host);
        if (!hostSsh) continue;

        try {
          dirTracker.remote(host, `Removing ${projectDir}`);
          const rmResult = await hostSsh.executeCommand(
            `rm -rf ${projectDir}`,
          );

          if (rmResult.success) {
            dirTracker.remote(host, `Removed ${projectDir}`);
          } else {
            log.warn(
              `Failed to remove ${projectDir} on ${host}: ${rmResult.stderr}`,
            );
          }
        } catch (error) {
          log.error(
            `Error removing ${projectDir} on ${host}: ${
              error instanceof Error ? error.message : String(error)
            }`,
          );
        }
      }

      dirTracker.finish();
      tracker.finish();

      console.log();
      log.success("Removal process completed");
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
