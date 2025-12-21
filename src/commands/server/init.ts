import { Command } from "@cliffy/command";
import { getEngineCommand } from "../../utils/config.ts";
import { setupCommandContext } from "../../utils/command_helpers.ts";
import { installEngineOnHosts } from "../../utils/engine.ts";
import { createServerAuditLogger } from "../../utils/audit.ts";
import { log, Logger } from "../../utils/logger.ts";
import { handleCommandError } from "../../utils/error_handler.ts";
import type { GlobalOptions } from "../../types.ts";
import { setupNetwork } from "../../lib/network/setup.ts";
import { DEFAULT_MAX_PREFIX_LENGTH } from "../../constants.ts";

export const initCommand = new Command()
  .description("Initialize servers")
  .action(async (options) => {
    const globalOptions = options as unknown as GlobalOptions;
    let ctx: Awaited<ReturnType<typeof setupCommandContext>> | undefined;

    try {
      await log.group("Server Initialization", async () => {
        log.info("Starting server initialization process", "init");

        // Set up command context (config, SSH, filtering)
        ctx = await setupCommandContext(globalOptions);

        const { config, sshManagers, targetHosts } = ctx;
        const configPath = config.configPath || "unknown";

        log.info(`Container engine: ${config.builder.engine}`, "engine");

        // Check if the specified engine is available
        const engineCommand = getEngineCommand(config);

        // Create audit logger for connected servers
        const auditLogger = createServerAuditLogger(
          sshManagers,
          config.project,
        );

        // Log initialization start to connected servers
        await auditLogger.logInitStart(
          targetHosts,
          config.builder.engine,
        );

        // Log configuration loading to connected servers
        await auditLogger.logConfigChange(configPath, "loaded");

        // Install engine on hosts
        await log.group(`${engineCommand} Installation`, async () => {
          log.status(
            `Installing ${engineCommand} on remote hosts...`,
            "install",
          );

          try {
            const installResults = await installEngineOnHosts(
              sshManagers,
              config.builder.engine,
            );

            // Create server loggers for individual host reporting
            const serverLoggers = Logger.forServers(targetHosts, {
              maxPrefixLength: DEFAULT_MAX_PREFIX_LENGTH,
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
              const hostSsh = sshManagers.find((ssh) =>
                ssh.getHost() === result.host
              );
              if (hostSsh) {
                const hostAuditLogger = createServerAuditLogger(
                  hostSsh,
                  config.project,
                );
                await hostAuditLogger.logEngineInstall(
                  config.builder.engine,
                  result.success ? "success" : "failed",
                  result.message ||
                    (result.success ? "Installation successful" : result.error),
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
            for (const host of targetHosts) {
              const hostSsh = sshManagers.find((ssh) => ssh.getHost() === host);
              if (hostSsh) {
                const hostLogger = createServerAuditLogger(
                  hostSsh,
                  config.project,
                );
                await hostLogger.logEngineInstall(
                  config.builder.engine,
                  "failed",
                  errorMessage,
                );
              }
            }

            log.status(`Continuing with initialization process...`, "init");
          }
        });

        // Network setup (if enabled)
        if (config.network.enabled) {
          try {
            const networkResults = await setupNetwork(config, sshManagers);

            // Log network setup results
            const successfulSetups = networkResults.filter((r) => r.success);
            const failedSetups = networkResults.filter((r) => !r.success);

            if (successfulSetups.length > 0) {
              log.success(
                `Private network configured on ${successfulSetups.length} server(s)`,
                "network",
              );
            }

            if (failedSetups.length > 0) {
              log.warn(
                `Network setup failed on ${failedSetups.length} server(s)`,
                "network",
              );
              for (const result of failedSetups) {
                log.error(
                  `${result.host}: ${result.error || "Unknown error"}`,
                  "network",
                );
              }
            }

            // Log network setup to audit trail
            for (const result of networkResults) {
              const hostSsh = sshManagers.find((ssh) =>
                ssh.getHost() === result.host
              );
              if (hostSsh) {
                const hostLogger = createServerAuditLogger(
                  hostSsh,
                  config.project,
                );
                await hostLogger.logCustomCommand(
                  "network_setup",
                  result.success ? "success" : "failed",
                  result.message || result.error,
                );
              }
            }
          } catch (error) {
            const errorMessage = error instanceof Error
              ? error.message
              : String(error);
            log.error(`Network setup failed: ${errorMessage}`, "network");

            // Log network setup failure
            for (const host of targetHosts) {
              const hostSsh = sshManagers.find((ssh) => ssh.getHost() === host);
              if (hostSsh) {
                const hostLogger = createServerAuditLogger(
                  hostSsh,
                  config.project,
                );
                await hostLogger.logCustomCommand(
                  "network_setup",
                  "failed",
                  errorMessage,
                );
              }
            }

            log.status(`Continuing with initialization process...`, "init");
          }
        }

        // Log successful initialization completion to connected servers
        const auditResults = await auditLogger.logInitSuccess(
          targetHosts,
          config.builder.engine,
        );

        // Report successful audit logging (should match connected hosts)
        const successfulHosts = auditResults
          .filter((result) => result.success)
          .map((result) => result.host);

        log.success(
          `Initialization completed successfully on ${targetHosts.length} server(s)`,
          "init",
        );
        log.info(
          `Audit trail updated on ${successfulHosts.length} server(s): ${
            successfulHosts.join(", ")
          }`,
          "audit",
        );
      });
    } catch (error) {
      await handleCommandError(error, {
        operation: "Initialization",
        component: "init",
        sshManagers: ctx?.sshManagers,
        projectName: ctx?.config?.project,
        targetHosts: ctx?.targetHosts,
        customAuditLogger: async (errorMessage) => {
          if (ctx?.sshManagers && ctx?.config && ctx?.targetHosts) {
            const auditLogger = createServerAuditLogger(
              ctx.sshManagers,
              ctx.config.project,
            );
            const failureResults = await auditLogger.logInitFailure(
              errorMessage,
              ctx.targetHosts,
              ctx.config.builder.engine,
            );

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
        },
      });
    } finally {
      if (ctx?.sshManagers) {
        const { cleanupSSHConnections } = await import(
          "../../utils/command_helpers.ts"
        );
        cleanupSSHConnections(ctx.sshManagers);
      }
    }
  });
