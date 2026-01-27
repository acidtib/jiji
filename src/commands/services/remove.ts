import { Command } from "@cliffy/command";
import { Confirm } from "@cliffy/prompt";
import {
  cleanupSSHConnections,
  executeBestEffort,
  findSSHManagerByHost,
  setupCommandContext,
} from "../../utils/command_helpers.ts";
import { handleCommandError } from "../../utils/error_handler.ts";
import { getTreePrefix, log } from "../../utils/logger.ts";
import { ProxyCommands } from "../../utils/proxy.ts";
import { unregisterContainerFromNetwork } from "../../lib/services/container_registry.ts";
import type { GlobalOptions } from "../../types.ts";

export const removeCommand = new Command()
  .description("Remove services from servers")
  .option("-y, --confirmed", "Skip confirmation prompt", { default: false })
  .option("--volume", "Also remove named volumes associated with services", {
    default: false,
  })
  .action(async (options) => {
    const globalOptions = options as unknown as GlobalOptions;
    let ctx: Awaited<ReturnType<typeof setupCommandContext>> | undefined;

    try {
      log.section("Service Removal:");

      // Load configuration first (before confirmation)
      const { Configuration } = await import("../../lib/configuration.ts");
      const config = await Configuration.load(
        globalOptions.environment,
        globalOptions.configFile,
      );
      const configPath = config.configPath || "unknown";

      // We need to resolve what we are removing BEFORE confirmation
      let allHosts = config.getAllServerHosts();
      let servicesToRemove: string[] = [];
      let isPartialRemoval = false;

      if (globalOptions.services) {
        const requestedServices = globalOptions.services.split(",").map((s) =>
          s.trim()
        );
        const matchingServices = config.getMatchingServiceNames(
          requestedServices,
        );

        if (matchingServices.length === 0) {
          log.error(
            `No services found matching: ${requestedServices.join(", ")}`,
          );
          Deno.exit(1);
        }
        servicesToRemove = matchingServices;
        allHosts = config.getHostsFromServices(matchingServices);
        isPartialRemoval = true;
      } else {
        servicesToRemove = config.getServiceNames();
      }

      log.say(`Configuration loaded from: ${configPath}`, 1);
      log.say(`Container engine: ${config.builder.engine}`, 1);
      log.say(
        `Found ${allHosts.length} remote host(s): ${allHosts.join(", ")}`,
        1,
      );

      if (isPartialRemoval) {
        log.say(
          `Targeting specific services: ${servicesToRemove.join(", ")}`,
          1,
        );
      } else {
        log.say(`Targeting ALL services`, 1);
      }

      // Get confirmation unless --confirmed flag is passed
      const confirmed = options.confirmed as boolean;
      if (!confirmed) {
        console.log();
        if (isPartialRemoval) {
          log.warn(
            `This will remove ${servicesToRemove.length} service(s) from ${allHosts.length} server(s).`,
          );
          log.warn(`Services: ${servicesToRemove.join(", ")}`);
        } else {
          log.warn(
            `This will remove ALL services and .jiji/${config.project} from ${allHosts.length} server(s)`,
          );
        }
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
      // setupCommandContext handles service filtering internally if options.skipServiceFiltering is NOT set (default)
      // We passed globalOptions which might have 'services'.
      ctx = await setupCommandContext(globalOptions);
      const { config: ctxConfig, sshManagers, targetHosts } = ctx;

      // Display SSH connection status
      console.log("");
      for (const ssh of sshManagers) {
        log.remote(ssh.getHost(), ": Connected", { indent: 1 });
      }

      // Filter services based on context match
      // If we are doing partial removal, ctx.matchingServices should be populated by setupCommandContext
      const finalServices = ctx.matchingServices
        ? ctx.matchingServices.map((name) => ctxConfig.services.get(name)!)
        : Array.from(ctxConfig.services.values());

      // Remove containers and proxy configuration for each service
      log.section("Removing Services:");

      for (const service of finalServices) {
        log.say(`- Removing ${service.name}`, 1);

        const resolvedServers = ctxConfig.getResolvedServersForService(
          service.name,
        );
        for (const server of resolvedServers) {
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
              const shouldRemoveVolumes = options.volume as boolean &&
                namedVolumes.length > 0;
              const isLastItem = !shouldRemoveVolumes; // If not removing volumes, container is the last item

              // Remove service from proxy if proxy is configured
              if (service.proxy?.enabled) {
                try {
                  const proxyCmd = new ProxyCommands(
                    ctxConfig.builder.engine,
                    hostSsh,
                  );

                  // For multi-target services, remove each target from proxy
                  const targets = service.proxy.targets;
                  for (const target of targets) {
                    const targetServiceName =
                      `${ctxConfig.project}-${service.name}-${target.app_port}`;
                    await proxyCmd.remove(targetServiceName);
                    log.say(
                      `├── Removed ${targetServiceName} from proxy`,
                      2,
                    );
                  }
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

              // Also remove any _old_* containers from failed/incomplete deployments
              // These are created during zero-downtime deployments and may be left behind
              const listOldCmd =
                `${ctxConfig.builder.engine} ps -a --filter "name=^${containerName}_old_" --format "{{.Names}}"`;
              const listResult = await hostSsh.executeCommand(listOldCmd);

              if (listResult.success && listResult.stdout.trim()) {
                const oldContainers = listResult.stdout.trim().split("\n");
                for (const oldContainer of oldContainers) {
                  await executeBestEffort(
                    hostSsh,
                    `${ctxConfig.builder.engine} rm -f ${oldContainer}`,
                    `removing old container ${oldContainer}`,
                  );
                  log.say(`├── Removed old container ${oldContainer}`, 2);
                }
              }

              // Remove named volumes (only if --volume flag is passed)
              if (shouldRemoveVolumes) {
                for (let i = 0; i < namedVolumes.length; i++) {
                  const volumeName = namedVolumes[i];
                  // Volume names are prefixed with service name during deployment
                  const prefixedVolumeName = `${service.name}-${volumeName}`;
                  const prefix = getTreePrefix(i, namedVolumes.length);

                  await executeBestEffort(
                    hostSsh,
                    `${ctxConfig.builder.engine} volume rm ${prefixedVolumeName}`,
                    `removing volume ${prefixedVolumeName}`,
                  );
                  log.say(`${prefix} Removed volume ${prefixedVolumeName}`, 2);
                }
              }
            } catch (error) {
              log.say(`└── Failed to remove ${service.name}: ${error}`, 2);
            }
          }, { indent: 1 });
        }
      }

      // Remove .jiji/project directory from all hosts ONLY if it is a FULL removal
      // We check if globalOptions.services was present to determine this state
      if (!isPartialRemoval) {
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
      } else {
        log.info(
          "Skipping project directory removal (partial service removal)",
        );
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
