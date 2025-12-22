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
      await log.group("Service Removal", async () => {
        // Load configuration first (before confirmation)
        const { Configuration } = await import("../lib/configuration.ts");
        const config = await Configuration.load(
          globalOptions.environment,
          globalOptions.configFile,
        );
        const configPath = config.configPath || "unknown";
        log.success(`Configuration loaded from: ${configPath}`, "config");
        log.info(`Project: ${config.project}`, "config");

        // Collect all unique hosts
        const allHosts = config.getAllServerHosts();

        if (allHosts.length === 0) {
          log.error(
            `No remote hosts found in configuration at: ${configPath}`,
            "config",
          );
          Deno.exit(1);
        }

        log.info(
          `Found ${allHosts.length} remote host(s): ${allHosts.join(", ")}`,
          "remove",
        );

        // Get confirmation unless --confirmed flag is passed
        const confirmed = options.confirmed as boolean;
        if (!confirmed) {
          console.log();
          log.warn(
            `This will remove all services and .jiji/${config.project} from ${allHosts.length} server(s)`,
            "remove",
          );
          console.log();

          const confirm = await Confirm.prompt({
            message: "Are you sure you want to proceed?",
            default: false,
          });

          if (!confirm) {
            log.info("Removal cancelled by user", "remove");
            return;
          }
        }

        log.info("Starting removal process...", "remove");

        // Set up command context
        ctx = await setupCommandContext(globalOptions);
        const { config: ctxConfig, sshManagers, targetHosts } = ctx;

        // Get all services
        const services = Array.from(ctxConfig.services.values());

        // Remove containers and proxy configuration for each service
        await log.group("Removing Services", async () => {
          for (const service of services) {
            log.status(`Removing service: ${service.name}`, "remove");

            for (const server of service.servers) {
              const host = server.host;
              if (!targetHosts.includes(host)) {
                log.warn(
                  `Skipping ${service.name} on unreachable host: ${host}`,
                  "remove",
                );
                continue;
              }

              const hostSsh = sshManagers.find((ssh) => ssh.getHost() === host);
              if (!hostSsh) continue;

              try {
                const containerName = service.getContainerName();

                // Remove service from proxy if proxy is configured
                if (service.proxy?.enabled) {
                  log.status(
                    `Removing ${service.name} from proxy on ${host}`,
                    "remove",
                  );
                  const proxyCmd = new ProxyCommands(
                    ctxConfig.builder.engine,
                    hostSsh,
                  );
                  await proxyCmd.remove(service.name);
                  log.success(
                    `Removed ${service.name} from proxy on ${host}`,
                    "remove",
                  );
                }

                // Unregister from network (if enabled)
                if (ctxConfig.network.enabled) {
                  try {
                    log.status(
                      `Unregistering ${service.name} from network...`,
                      "network",
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
                      "network",
                    );
                  }
                }

                // Stop and remove the container
                log.status(
                  `Removing container ${containerName} on ${host}`,
                  "remove",
                );
                await executeBestEffort(
                  hostSsh,
                  `${ctxConfig.builder.engine} rm -f ${containerName}`,
                  `removing container ${containerName}`,
                );
                log.success(
                  `Removed container ${containerName} on ${host}`,
                  "remove",
                );

                const namedVolumes = service.getNamedVolumes();
                if (namedVolumes.length > 0) {
                  log.status(
                    `Removing ${namedVolumes.length} named volume(s) for ${service.name} on ${host}`,
                    "remove",
                  );

                  for (const volumeName of namedVolumes) {
                    await executeBestEffort(
                      hostSsh,
                      `${ctxConfig.builder.engine} volume rm ${volumeName}`,
                      `removing volume ${volumeName}`,
                    );
                    log.success(
                      `Removed volume ${volumeName} on ${host}`,
                      "remove",
                    );
                  }
                }
              } catch (error) {
                log.error(
                  `Failed to remove ${service.name} on ${host}: ${
                    error instanceof Error ? error.message : String(error)
                  }`,
                  "remove",
                );
              }
            }
          }
        });

        // Remove .jiji/project directory from all hosts
        await log.group("Removing Project Directory", async () => {
          const projectDir = `.jiji/${ctxConfig.project}`;
          log.info(`Removing ${projectDir} from all hosts`, "remove");

          for (const host of targetHosts) {
            const hostSsh = sshManagers.find((ssh) => ssh.getHost() === host);
            if (!hostSsh) continue;

            try {
              log.status(`Removing ${projectDir} on ${host}`, "remove");
              const rmResult = await hostSsh.executeCommand(
                `rm -rf ${projectDir}`,
              );

              if (rmResult.success) {
                log.success(`Removed ${projectDir} on ${host}`, "remove");
              } else {
                log.warn(
                  `Failed to remove ${projectDir} on ${host}: ${rmResult.stderr}`,
                  "remove",
                );
              }
            } catch (error) {
              log.error(
                `Error removing ${projectDir} on ${host}: ${
                  error instanceof Error ? error.message : String(error)
                }`,
                "remove",
              );
            }
          }
        });

        log.success("Removal process completed", "remove");
      });
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
