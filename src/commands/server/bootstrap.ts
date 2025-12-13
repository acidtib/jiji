import { Command } from "@cliffy/command";
import { getEngineCommand } from "../../utils/config.ts";
import { Configuration } from "../../lib/configuration.ts";
import {
  createSSHConfigFromJiji,
  createSSHManagers,
  filterConnectedHosts,
  testConnections,
  validateSSHSetup,
} from "../../utils/ssh.ts";
import { installEngineOnHosts } from "../../utils/engine.ts";
import { createServerAuditLogger } from "../../utils/audit.ts";
import { log, Logger } from "../../utils/logger.ts";
import type { GlobalOptions } from "../../types.ts";

export const bootstrapCommand = new Command()
  .description("Bootstrap servers")
  .action(async (options) => {
    let uniqueHosts: string[] = [];
    let config: Configuration | undefined;
    let auditLogger: ReturnType<typeof createServerAuditLogger> | undefined;
    let sshManagers: ReturnType<typeof createSSHManagers> | undefined;
    let installResults:
      | Awaited<ReturnType<typeof installEngineOnHosts>>
      | undefined;

    try {
      await log.group("Server Bootstrap", async () => {
        log.info("Starting server bootstrap process", "bootstrap");

        // Cast options to GlobalOptions to access global options
        const globalOptions = options as unknown as GlobalOptions;

        // Load and parse the configuration using new system
        config = await Configuration.load(
          globalOptions.environment,
          globalOptions.configFile,
        );
        const configPath = config.configPath || "unknown";
        log.success(`Configuration loaded from: ${configPath}`, "config");

        // Configuration loading will be logged once we have SSH connections
        log.info(`Container engine: ${config.engine}`, "engine");

        // Check if the specified engine is available
        const engineCommand = getEngineCommand(config);

        // Collect all unique hosts from services using new system
        const allHosts = config.getAllHosts();

        uniqueHosts = allHosts;

        if (uniqueHosts.length > 0) {
          log.info(
            `Found ${uniqueHosts.length} remote host(s): ${
              uniqueHosts.join(", ")
            }`,
            "bootstrap",
          );

          await log.group("SSH Connection Setup", async () => {
            // Validate SSH setup
            log.status("Validating SSH configuration", "ssh");
            const sshValidation = await validateSSHSetup();
            if (!sshValidation.valid) {
              log.error(`SSH setup validation failed:`, "ssh");
              log.error(`   ${sshValidation.message}`, "ssh");
              log.error(
                `   Please run 'ssh-agent' and 'ssh-add' before continuing.`,
                "ssh",
              );
              Deno.exit(1);
            }
            log.success("SSH setup validation passed", "ssh");

            // Get SSH configuration from config file only
            const sshConfig = {
              ...createSSHConfigFromJiji({
                user: config!.ssh.user,
                port: config!.ssh.port,
              }),
              useAgent: true,
            };

            // Create SSH managers for all hosts and test connections
            log.status("Testing connections to all hosts...", "ssh");
            sshManagers = createSSHManagers(uniqueHosts, sshConfig);
            const connectionTests = await testConnections(sshManagers);

            const { connectedManagers, connectedHosts, failedHosts } =
              filterConnectedHosts(sshManagers, connectionTests);

            if (connectedHosts.length === 0) {
              log.error(
                "❌ No hosts are reachable. Cannot proceed with bootstrap.",
                "ssh",
              );
              Deno.exit(1);
            }

            if (failedHosts.length > 0) {
              log.warn(
                `Skipping unreachable hosts: ${failedHosts.join(", ")}`,
                "ssh",
              );
            }

            log.success(
              `Connected to ${connectedHosts.length} host(s): ${
                connectedHosts.join(", ")
              }`,
              "ssh",
            );

            // Use only connected SSH managers
            sshManagers = connectedManagers;
            uniqueHosts = connectedHosts;
          });

          // Create audit logger for connected servers only
          auditLogger = createServerAuditLogger(sshManagers!);

          // Log bootstrap start to connected servers
          await auditLogger.logBootstrapStart(
            uniqueHosts,
            config!.engine,
          );

          // Log configuration loading to connected servers
          await auditLogger.logConfigChange(configPath, "loaded");

          await log.group(`${engineCommand} Installation`, async () => {
            log.status(
              `Installing ${engineCommand} on remote hosts...`,
              "install",
            );

            try {
              installResults = await installEngineOnHosts(
                sshManagers!,
                config!.engine,
              );

              // Create server loggers for individual host reporting
              const serverLoggers = Logger.forServers(uniqueHosts, {
                maxPrefixLength: 25,
              });

              // Log engine installation results to each respective server
              for (const result of installResults) {
                const hostLogger = serverLoggers.get(result.host);
                if (hostLogger) {
                  if (result.success) {
                    hostLogger.success(
                      result.message || "Installation successful",
                    );
                  } else {
                    hostLogger.error(
                      result.error || result.message || "Installation failed",
                    );
                  }
                }

                // Also log to audit trail
                const hostSsh = sshManagers!.find((
                  ssh: ReturnType<typeof createSSHManagers>[0],
                ) => ssh.getHost() === result.host);
                if (hostSsh) {
                  const hostAuditLogger = createServerAuditLogger(hostSsh);
                  await hostAuditLogger.logEngineInstall(
                    config!.engine,
                    result.success ? "success" : "failed",
                    result.message ||
                      (result.success
                        ? "Installation successful"
                        : result.error),
                  );
                }
              }

              const successfulInstalls = installResults.filter((r) =>
                r.success
              ).length;
              const failedInstalls = installResults.filter((r) =>
                !r.success
              ).length;

              if (successfulInstalls > 0) {
                log.success(
                  `${engineCommand} installed successfully on ${successfulInstalls} host(s)`,
                  "install",
                );
              }

              if (failedInstalls > 0) {
                log.warn(
                  `${engineCommand} installation failed on ${failedInstalls} host(s)`,
                  "install",
                );
              }
            } catch (error) {
              const errorMessage = error instanceof Error
                ? error.message
                : String(error);
              log.error(
                `Engine installation failed: ${errorMessage}`,
                "install",
              );

              // Log engine installation failure
              if (auditLogger && sshManagers) {
                for (const host of uniqueHosts) {
                  const hostSsh = sshManagers.find((
                    ssh: ReturnType<typeof createSSHManagers>[0],
                  ) => ssh.getHost() === host);
                  if (hostSsh) {
                    const hostLogger = createServerAuditLogger(hostSsh);
                    await hostLogger.logEngineInstall(
                      config!.engine,
                      "failed",
                      errorMessage,
                    );
                  }
                }
              }

              log.status(`Continuing with bootstrap process...`, "bootstrap");
            }
          });
        } else {
          log.error(
            `No remote hosts found in configuration at: ${configPath}`,
            "config",
          );
          log.error(
            `Could not find any hosts. Please update your jiji config to include hosts for services.`,
            "config",
          );
          Deno.exit(1);
        }

        // Log successful bootstrap completion to connected servers
        if (auditLogger) {
          const auditResults = await auditLogger.logBootstrapSuccess(
            uniqueHosts,
            config!.engine,
          );

          // Report successful audit logging (should match connected hosts)
          const successfulHosts = auditResults
            .filter((result) => result.success)
            .map((result) => result.host);

          log.success(
            `Bootstrap completed successfully on ${uniqueHosts.length} server(s)`,
            "bootstrap",
          );
          log.info(
            `📋 Audit trail updated on ${successfulHosts.length} server(s): ${
              successfulHosts.join(", ")
            }`,
            "audit",
          );
        }
      });
    } catch (error) {
      const errorMessage = error instanceof Error
        ? error.message
        : String(error);
      log.error("Bootstrap failed:", "bootstrap");
      log.error(errorMessage, "bootstrap");

      // Log bootstrap failure to servers if possible
      if (auditLogger) {
        const failureResults = await auditLogger.logBootstrapFailure(
          errorMessage,
          uniqueHosts,
          config?.engine,
        );

        // Report which servers received the failure log
        const successfulFailureLogs = failureResults
          .filter((result) => result.success)
          .map((result) => result.host);

        if (successfulFailureLogs.length > 0) {
          log.info(
            `Failure logged to ${successfulFailureLogs.length} server(s): ${
              successfulFailureLogs.join(", ")
            }`,
            "audit",
          );
        }
      }

      Deno.exit(1);
    } finally {
      // Always clean up SSH connections to prevent hanging
      if (sshManagers) {
        sshManagers.forEach((ssh) => {
          try {
            ssh.dispose();
          } catch (error) {
            // Ignore cleanup errors, but log them for debugging
            log.debug(`Failed to dispose SSH connection: ${error}`, "ssh");
          }
        });
      }
    }
  });
