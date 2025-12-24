import { Command } from "@cliffy/command";
import {
  cleanupSSHConnections,
  setupCommandContext,
} from "../../utils/command_helpers.ts";
import { createServerAuditLogger } from "../../utils/audit.ts";
import { log, Logger } from "../../utils/logger.ts";
import { executeHostOperations } from "../../utils/promise_helpers.ts";
import { handleCommandError } from "../../utils/error_handler.ts";
import { DEFAULT_MAX_PREFIX_LENGTH } from "../../constants.ts";
import type { GlobalOptions } from "../../types.ts";

export const execCommand = new Command()
  .description("Execute a custom command on remote hosts")
  .arguments("<command:string>")
  .option(
    "-i, --interactive",
    "Run the command interactively (use for console/bash)",
    {
      default: false,
    },
  )
  .option("--parallel", "Execute commands in parallel on all hosts", {
    default: false,
  })
  .option(
    "--continue-on-error",
    "Continue executing on other hosts even if one fails",
    { default: true },
  )
  .option(
    "--timeout <seconds:number>",
    "Command timeout in seconds (default: 300)",
    { default: 300 },
  )
  .action(async (options, command: string) => {
    const globalOptions = options as unknown as GlobalOptions;
    let ctx: Awaited<ReturnType<typeof setupCommandContext>> | undefined;

    try {
      log.section("Remote Command Execution:");
      log.say(`- Command: ${command}`, 1);

      const { Configuration } = await import("../../lib/configuration.ts");
      const config = await Configuration.load(
        globalOptions.environment,
        globalOptions.configFile,
      );

      const configPath = config.configPath || "unknown";
      const allHosts = config.getAllServerHosts();

      log.say(`- Configuration loaded from: ${configPath}`, 1);
      log.say(`- Container engine: ${config.builder.engine}`, 1);
      log.say(
        `- Found ${allHosts.length} remote host(s): ${allHosts.join(", ")}`,
        1,
      );

      // Set up command context
      ctx = await setupCommandContext(globalOptions, {
        allowPartialConnection: options.continueOnError,
      });

      const { sshManagers, targetHosts } = ctx;

      // Show connection status for each host
      console.log(""); // Empty line
      for (const ssh of sshManagers) {
        log.remote(ssh.getHost(), ": Connected", { indent: 1 });
      }

      // Create audit logger
      const auditLogger = createServerAuditLogger(
        sshManagers,
        config.project,
      );

      log.section("Execution Options:");
      log.say(`- Interactive mode: ${options.interactive}`, 1);
      log.say(
        `- Execution mode: ${options.parallel ? "parallel" : "sequential"}`,
        1,
      );
      log.say(`- Timeout: ${options.timeout} seconds`, 1);
      log.say(`- Continue on error: ${options.continueOnError}`, 1);

      // Handle interactive mode
      if (options.interactive) {
        if (sshManagers.length > 1) {
          log.error(
            "Interactive mode only supports single host execution",
          );
          log.say(
            `Found ${sshManagers.length} hosts. Please specify a single host for interactive mode.`,
            1,
          );
          return;
        }

        log.section("Starting Interactive Session:");
        log.say(`- Host: ${sshManagers[0].getHost()}`, 1);
        const ssh = sshManagers[0];

        try {
          await ssh.startInteractiveSession(command);
          Deno.exit(0);
        } catch (error) {
          const errorMessage = error instanceof Error
            ? error.message
            : String(error);
          log.error(`Interactive session failed: ${errorMessage}`);
          Deno.exit(1);
        }
      }

      // Create server loggers for individual host reporting
      const serverLoggers = Logger.forServers(targetHosts, {
        maxPrefixLength: DEFAULT_MAX_PREFIX_LENGTH,
      });

      // Execute the command on all hosts
      const executionResults = [];

      log.section("Executing Command:");

      if (options.parallel) {
        log.say("- Running in parallel mode", 1);

        // Create host operations
        const hostOperations = sshManagers.map((ssh) => ({
          host: ssh.getHost(),
          operation: async () => {
            return await executeCommandWithTimeout(
              ssh,
              command,
              options.timeout,
              serverLoggers,
            );
          },
        }));

        // Execute with error collection
        const aggregatedResults = await executeHostOperations(
          hostOperations,
        );

        // Combine successful results with failed operations
        const results = [...aggregatedResults.results];

        // Convert failed operations to execution result format
        for (const { host, error } of aggregatedResults.hostErrors) {
          const hostLogger = serverLoggers.get(host);
          if (hostLogger) {
            hostLogger.error(`Command execution failed: ${error.message}`);
          }

          results.push({
            host,
            success: false,
            code: -1,
            stdout: "",
            stderr: error.message,
          });
        }

        executionResults.push(...results);
      } else {
        log.say("- Running in sequential mode", 1);

        // Execute sequentially
        for (const ssh of sshManagers) {
          const host = ssh.getHost();

          try {
            const result = await executeCommandWithTimeout(
              ssh,
              command,
              options.timeout,
              serverLoggers,
            );

            executionResults.push({
              host,
              result,
              success: result.success,
            });

            // Stop on first failure if continue-on-error is false
            if (!result.success && !options.continueOnError) {
              log.error(
                `Command failed on ${host}, stopping execution`,
                "exec",
              );
              break;
            }
          } catch (error) {
            const errorMessage = error instanceof Error
              ? error.message
              : String(error);

            const hostLogger = serverLoggers.get(host);
            if (hostLogger) {
              hostLogger.error(`Command failed: ${errorMessage}`);
            }

            executionResults.push({
              host,
              result: {
                stdout: "",
                stderr: errorMessage,
                success: false,
                code: null,
              },
              success: false,
              error: errorMessage,
            });

            // Stop on first failure if continue-on-error is false
            if (!options.continueOnError) {
              log.error(
                `Command failed on ${host}, stopping execution`,
                "exec",
              );
              break;
            }
          }
        }
      }

      // Summary
      const successful = executionResults.filter((r) => r.success);
      const failed = executionResults.filter((r) => !r.success);

      log.section("Execution Summary:");
      log.say(
        `- Successful: ${successful.length} host(s)${
          successful.length > 0
            ? ` - ${successful.map((r) => r.host).join(", ")}`
            : ""
        }`,
        1,
      );

      if (failed.length > 0) {
        log.say(
          `- Failed: ${failed.length} host(s) - ${
            failed.map((r) => r.host).join(", ")
          }`,
          1,
        );
      }

      // Overall success/failure
      if (failed.length > 0 && !options.continueOnError) {
        // Log failures to audit before exiting
        await auditLogger.logCustomCommand(
          command,
          "failed",
          `Command execution failed on ${failed.length} host(s): ${
            failed.map((r) => r.host).join(", ")
          }`,
        );

        log.error("\nCommand execution failed on some hosts");
        Deno.exit(1);
      } else if (failed.length === 0) {
        // Log completion to audit
        await auditLogger.logCustomCommand(
          command,
          "success",
          `Command executed successfully on all ${successful.length} host(s)`,
        );

        log.success("\nRemote command execution completed!", 0);
      } else {
        // Log completion to audit
        await auditLogger.logCustomCommand(
          command,
          "failed",
          `Command completed with ${failed.length} failure(s) out of ${executionResults.length} host(s)`,
        );

        log.warn(
          `\nCommand completed with some failures (${failed.length}/${executionResults.length} hosts failed)`,
        );
      }
    } catch (error) {
      await handleCommandError(error, {
        operation: "Command execution",
        component: "exec",
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

/**
 * Execute a command with timeout
 */
async function executeCommandWithTimeout(
  ssh: Awaited<
    ReturnType<typeof import("../../utils/ssh.ts").setupSSHConnections>
  >["managers"][0],
  command: string,
  timeoutSeconds: number,
  serverLoggers: Map<string, Logger>,
) {
  const host = ssh.getHost();
  const hostLogger = serverLoggers.get(host);

  if (hostLogger) {
    hostLogger.executing(command);
  }

  let timerId: number | undefined;

  try {
    // Create a timeout promise
    const timeoutPromise = new Promise((_, reject) => {
      timerId = setTimeout(
        () =>
          reject(
            new Error(
              `Command timed out after ${timeoutSeconds} seconds`,
            ),
          ),
        timeoutSeconds * 1000,
      );
    });

    // Race the command execution against the timeout
    const result = await Promise.race([
      ssh.executeCommand(command),
      timeoutPromise,
    ]) as Awaited<ReturnType<typeof ssh.executeCommand>>;

    // Format output for host
    let output = "";
    if (result.success) {
      output += `Command completed (exit code: ${result.code})\n`;
    } else {
      output += `Command failed (exit code: ${result.code})\n`;
    }

    if (result.stdout.trim()) {
      output += result.stdout.trim() + "\n";
    }
    if (result.stderr.trim()) {
      output += "STDERR:\n" + result.stderr.trim() + "\n";
    }

    // Use host-grouped output
    log.hostOutput(host, output.trim(), { type: "Exec" });

    return {
      host,
      success: result.success,
      code: result.code,
      stdout: result.stdout,
      stderr: result.stderr,
    };
  } finally {
    if (timerId !== undefined) {
      clearTimeout(timerId);
    }
  }
}
