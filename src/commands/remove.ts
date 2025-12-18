import { Command } from "@cliffy/command";
import { Confirm } from "@cliffy/prompt";
import { Configuration } from "../lib/configuration.ts";
import { setupSSHConnections, type SSHManager } from "../utils/ssh.ts";
import { log } from "../utils/logger.ts";
import { ProxyCommands } from "../utils/proxy.ts";
import type { GlobalOptions } from "../types.ts";
import type { ServiceConfiguration } from "../lib/configuration/service.ts";

/**
 * Extracts named volumes from a service's volume configuration.
 * Named volumes are those that don't start with "/" or "./" (not host paths).
 * Format: "volume_name:/container/path" or "volume_name:/container/path:options"
 */
function getNamedVolumes(service: ServiceConfiguration): string[] {
  const namedVolumes: string[] = [];

  for (const volume of service.volumes) {
    const parts = volume.split(":");
    if (parts.length >= 2) {
      const source = parts[0];
      // Named volumes don't start with "/" or "./" (host paths do)
      if (!source.startsWith("/") && !source.startsWith("./")) {
        namedVolumes.push(source);
      }
    }
  }

  return namedVolumes;
}

export const removeCommand = new Command()
  .description("Remove services and .jiji/project_dir from servers")
  .option("-y, --confirmed", "Skip confirmation prompt", { default: false })
  .action(async (options) => {
    let uniqueHosts: string[] = [];
    let config: Configuration | undefined;
    let sshManagers: SSHManager[] | undefined;

    try {
      await log.group("Service Removal", async () => {
        // Cast options to GlobalOptions
        const globalOptions = options as unknown as GlobalOptions;

        // Load configuration
        config = await Configuration.load(
          globalOptions.environment,
          globalOptions.configFile,
        );
        const configPath = config.configPath || "unknown";
        log.success(`Configuration loaded from: ${configPath}`, "config");
        log.info(`Project: ${config.project}`, "config");

        if (!config) throw new Error("Configuration failed to load");

        // Collect all unique hosts
        const allHosts = config.getAllServerHosts();
        uniqueHosts = allHosts;

        if (uniqueHosts.length === 0) {
          log.error(
            `No remote hosts found in configuration at: ${configPath}`,
            "config",
          );
          Deno.exit(1);
        }

        log.info(
          `Found ${uniqueHosts.length} remote host(s): ${
            uniqueHosts.join(", ")
          }`,
          "remove",
        );

        // Get confirmation unless --confirmed flag is passed
        const confirmed = options.confirmed as boolean;
        if (!confirmed) {
          console.log();
          log.warn(
            `This will remove all services and .jiji/${config.project} from ${uniqueHosts.length} server(s)`,
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

        await log.group("SSH Connection Setup", async () => {
          const result = await setupSSHConnections(
            uniqueHosts,
            {
              user: config!.ssh.user,
              port: config!.ssh.port,
              proxy: config!.ssh.proxy,
              proxy_command: config!.ssh.proxyCommand,
              keys: config!.ssh.allKeys.length > 0
                ? config!.ssh.allKeys
                : undefined,
              keyData: config!.ssh.keyData,
              keysOnly: config!.ssh.keysOnly,
              dnsRetries: config!.ssh.dnsRetries,
            },
            { allowPartialConnection: true },
          );

          sshManagers = result.managers;
          uniqueHosts = result.connectedHosts;
        });

        // Get all services
        const services = Array.from(config.services.values());

        // Remove containers and proxy configuration for each service
        await log.group("Removing Services", async () => {
          for (const service of services) {
            log.status(`Removing service: ${service.name}`, "remove");

            for (const server of service.servers) {
              const host = server.host;
              if (!uniqueHosts.includes(host)) {
                log.warn(
                  `Skipping ${service.name} on unreachable host: ${host}`,
                  "remove",
                );
                continue;
              }

              const hostSsh = sshManagers!.find((ssh) =>
                ssh.getHost() === host
              );
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
                    config!.builder.engine,
                    hostSsh,
                  );
                  await proxyCmd.remove(service.name);
                  log.success(
                    `Removed ${service.name} from proxy on ${host}`,
                    "remove",
                  );
                }

                // Stop and remove the container
                log.status(
                  `Removing container ${containerName} on ${host}`,
                  "remove",
                );
                const rmResult = await hostSsh.executeCommand(
                  `${
                    config!.builder.engine
                  } rm -f ${containerName} 2>/dev/null || true`,
                );

                if (rmResult.success) {
                  log.success(
                    `Removed container ${containerName} on ${host}`,
                    "remove",
                  );
                }

                // Remove named volumes for this service
                const namedVolumes = getNamedVolumes(service);
                if (namedVolumes.length > 0) {
                  log.status(
                    `Removing ${namedVolumes.length} named volume(s) for ${service.name} on ${host}`,
                    "remove",
                  );

                  for (const volumeName of namedVolumes) {
                    try {
                      const volResult = await hostSsh.executeCommand(
                        `${
                          config!.builder.engine
                        } volume rm ${volumeName} 2>/dev/null || true`,
                      );

                      if (volResult.success) {
                        log.success(
                          `Removed volume ${volumeName} on ${host}`,
                          "remove",
                        );
                      }
                    } catch (volError) {
                      log.warn(
                        `Failed to remove volume ${volumeName} on ${host}: ${
                          volError instanceof Error
                            ? volError.message
                            : String(volError)
                        }`,
                        "remove",
                      );
                    }
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
          const projectDir = `.jiji/${config!.project}`;
          log.info(`Removing ${projectDir} from all hosts`, "remove");

          for (const host of uniqueHosts) {
            const hostSsh = sshManagers!.find((ssh) => ssh.getHost() === host);
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
      const errorMessage = error instanceof Error
        ? error.message
        : String(error);
      log.error("Removal failed:", "remove");
      log.error(errorMessage, "remove");
      Deno.exit(1);
    } finally {
      // Clean up SSH connections
      if (sshManagers) {
        sshManagers.forEach((ssh) => {
          try {
            ssh.dispose();
          } catch (error) {
            log.debug(`Failed to dispose SSH connection: ${error}`, "ssh");
          }
        });
      }
    }
  });
