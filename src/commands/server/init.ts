import { Command } from "@cliffy/command";
import { getEngineCommand } from "../../utils/config.ts";
import { setupCommandContext } from "../../utils/command_helpers.ts";
import { installEngineOnHosts } from "../../utils/engine.ts";
import { createServerAuditLogger } from "../../utils/audit.ts";
import { log } from "../../utils/logger.ts";
import { handleCommandError } from "../../utils/error_handler.ts";
import type { GlobalOptions } from "../../types.ts";
import { setupNetwork } from "../../lib/network/setup.ts";

export const initCommand = new Command()
  .description("Initialize servers")
  .action(async (options) => {
    const globalOptions = options as unknown as GlobalOptions;
    let ctx: Awaited<ReturnType<typeof setupCommandContext>> | undefined;

    try {
      log.section("Server Initialization");
      log.say("Starting server initialization process");

      // Set up command context (config, SSH, filtering)
      ctx = await setupCommandContext(globalOptions);

      const { config, sshManagers, targetHosts } = ctx;
      const configPath = config.configPath || "unknown";

      log.say(`Container engine: ${config.builder.engine}`);

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
      const installTracker = log.createStepTracker(
        `${engineCommand} Installation`,
      );

      installTracker.step(
        `Installing ${engineCommand} on ${targetHosts.length} host(s)...`,
      );

      try {
        const installResults = await installEngineOnHosts(
          sshManagers,
          config.builder.engine,
          installTracker,
        );

        // Log engine installation results to each respective server
        for (const result of installResults) {
          if (result.success) {
            installTracker.remote(
              result.host,
              result.message || "Installation successful",
            );
          } else {
            installTracker.remote(
              result.host,
              `Failed: ${
                result.error || result.message || "Installation failed"
              }`,
            );
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

        const failedInstalls = installResults.filter((r) => !r.success).length;

        if (failedInstalls > 0) {
          const msg =
            `${engineCommand} installation failed on ${failedInstalls} host(s)`;
          installTracker.finish(false);
          throw new Error(msg);
        }

        installTracker.finish();
      } catch (error) {
        const errorMessage = error instanceof Error
          ? error.message
          : String(error);

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

        throw error;
      }

      // Network setup (if enabled)
      if (config.network.enabled) {
        try {
          const networkResults = await setupNetwork(config, sshManagers);

          // Log network setup results
          const successfulSetups = networkResults.filter((r) => r.success);
          const failedSetups = networkResults.filter((r) => !r.success);

          if (failedSetups.length > 0) {
            for (const result of failedSetups) {
              log.error(
                `${result.host}: ${result.error || "Unknown error"}`,
                "network",
              );
            }

            // Throw error for critical network component failures
            const criticalErrors = failedSetups.map((r) =>
              `${r.host}: ${r.error || "Network setup failed"}`
            ).join("; ");
            throw new Error(
              `Critical network setup failures: ${criticalErrors}`,
            );
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

          if (successfulSetups.length > 0) {
            log.say(
              `Private network configured on ${successfulSetups.length} server(s)`,
            );
          }
        } catch (error) {
          const errorMessage = error instanceof Error
            ? error.message
            : String(error);

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

          throw error;
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

      console.log();
      log.say(
        `Initialization completed successfully on ${targetHosts.length} server(s)`,
      );
      log.say(
        `Audit trail updated on ${successfulHosts.length} server(s): ${
          successfulHosts.join(", ")
        }`,
        1,
      );
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
              log.say(
                `Failure logged to ${successfulFailureLogs.length} server(s): ${
                  successfulFailureLogs.join(", ")
                }`,
                1,
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
