import { Command } from "@cliffy/command";
import { Configuration } from "../../lib/configuration.ts";
import {
  createSSHConfigFromJiji,
  createSSHManagers,
  filterConnectedHosts,
  testConnections,
  validateSSHSetup,
} from "../../utils/ssh.ts";
import { createServerAuditLogger } from "../../utils/audit.ts";
import { log, Logger } from "../../utils/logger.ts";
import type { GlobalOptions } from "../../types.ts";

export const execCommand = new Command()
  .description("Execute a custom command on remote hosts")
  .arguments("<command:string>")
  .option("--ssh-user <username:string>", "SSH username for remote hosts")
  .option("--ssh-key <path:string>", "Path to SSH private key")
  .option("--ssh-port <port:number>", "SSH port (default: 22)")
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
    let config: Configuration | undefined;
    let sshManagers: ReturnType<typeof createSSHManagers> | undefined;
    let auditLogger: ReturnType<typeof createServerAuditLogger> | undefined;
    let targetHosts: string[] = [];

    try {
      await log.group("Remote Command Execution", async () => {
        log.info(`Executing command: ${command}`, "exec");

        // Cast options to GlobalOptions to access global options
        const globalOptions = options as unknown as GlobalOptions;

        // Load and parse the configuration using new system
        config = await Configuration.load(
          globalOptions.environment,
          globalOptions.configFile,
        );
        const configPath = config.configPath || "unknown";
        log.success(`Configuration loaded from: ${configPath}`, "config");

        // Collect all unique hosts from services using new system
        const allHosts = config.getAllHosts();
        let uniqueHosts = allHosts;

        // Filter hosts if specific hosts are requested
        if (globalOptions.hosts) {
          const requestedHosts = globalOptions.hosts.split(",").map((h) =>
            h.trim()
          );
          const validHosts = requestedHosts.filter((host) =>
            uniqueHosts.includes(host)
          );
          const invalidHosts = requestedHosts.filter((host) =>
            !uniqueHosts.includes(host)
          );

          if (invalidHosts.length > 0) {
            log.warn(
              `Invalid hosts specified (not in config): ${
                invalidHosts.join(", ")
              }`,
              "exec",
            );
          }

          if (validHosts.length === 0) {
            log.error("No valid hosts specified", "exec");
            Deno.exit(1);
          }

          uniqueHosts = validHosts;
          log.info(
            `Targeting specific hosts: ${uniqueHosts.join(", ")}`,
            "exec",
          );
        }

        if (uniqueHosts.length === 0) {
          log.error(
            `No hosts found in configuration at: ${configPath}`,
            "config",
          );
          log.error(
            `Please update your jiji config to include hosts for services.`,
            "config",
          );
          Deno.exit(1);
        }

        log.info(
          `Found ${uniqueHosts.length} host(s): ${uniqueHosts.join(", ")}`,
          "exec",
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

          // Get SSH configuration from config file and command line options
          const baseSshConfig = createSSHConfigFromJiji({
            user: config!.ssh.user,
            port: config!.ssh.port,
          });
          const sshConfig = {
            username: options.sshUser || baseSshConfig.username,
            port: options.sshPort || baseSshConfig.port,
            useAgent: true,
          };

          // Create SSH managers for all hosts and test connections
          log.status("Testing connections to all hosts...", "ssh");
          sshManagers = createSSHManagers(uniqueHosts, sshConfig);
          const connectionTests = await testConnections(sshManagers);

          const { connectedManagers, connectedHosts, failedHosts } =
            filterConnectedHosts(sshManagers, connectionTests);

          if (connectedHosts.length === 0) {
            log.error("No hosts are reachable. Cannot execute command.", "ssh");
            Deno.exit(1);
          }

          if (failedHosts.length > 0) {
            log.warn(`Unreachable hosts: ${failedHosts.join(", ")}`, "ssh");
            if (!options.continueOnError) {
              log.error(
                "Stopping due to unreachable hosts (use --continue-on-error to override)",
                "ssh",
              );
              Deno.exit(1);
            }
          }

          log.success(
            `Connected to ${connectedHosts.length} host(s): ${
              connectedHosts.join(", ")
            }`,
            "ssh",
          );

          // Use only connected SSH managers
          sshManagers = connectedManagers;
          targetHosts = connectedHosts;
        });

        // Create audit logger for connected servers
        auditLogger = createServerAuditLogger(sshManagers!);

        await log.group("Command Execution", async () => {
          log.info(`Interactive mode: ${options.interactive}`, "exec");
          log.info(
            `Execution mode: ${options.parallel ? "parallel" : "sequential"}`,
            "exec",
          );
          log.info(`Timeout: ${options.timeout} seconds`, "exec");
          log.info(`Continue on error: ${options.continueOnError}`, "exec");

          // Handle interactive mode
          if (options.interactive) {
            if (sshManagers!.length > 1) {
              log.error(
                "Interactive mode only supports single host execution",
                "exec",
              );
              log.info(
                `Found ${
                  sshManagers!.length
                } hosts. Please specify a single host for interactive mode.`,
                "exec",
              );
              return;
            }

            log.status("Starting interactive session...", "exec");
            const ssh = sshManagers![0];

            try {
              await ssh.startInteractiveSession(command);
              // Interactive session completed, exit quietly
              Deno.exit(0);
            } catch (error) {
              const errorMessage = error instanceof Error
                ? error.message
                : String(error);
              log.error(`Interactive session failed: ${errorMessage}`, "exec");
              Deno.exit(1);
            }
          }

          // Create server loggers for individual host reporting
          const serverLoggers = Logger.forServers(targetHosts, {
            maxPrefixLength: 25,
          });

          // Execute the command on all hosts
          const executionResults = [];

          if (options.parallel) {
            log.status("Executing command in parallel...", "exec");

            // Execute in parallel
            const execPromises = sshManagers!.map(async (ssh) => {
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
                          `Command timed out after ${options.timeout} seconds`,
                        ),
                      ),
                    options.timeout * 1000,
                  );
                });

                // Race the command execution against the timeout
                const result = await Promise.race([
                  ssh.executeCommand(command),
                  timeoutPromise,
                ]) as Awaited<ReturnType<typeof ssh.executeCommand>>;

                if (hostLogger) {
                  if (result.success) {
                    hostLogger.success(
                      `Command completed (exit code: ${result.code})`,
                    );
                  } else {
                    hostLogger.error(
                      `Command failed (exit code: ${result.code})`,
                    );
                  }

                  if (result.stdout.trim()) {
                    result.stdout.trim().split("\n").forEach((line) => {
                      hostLogger.info(`STDOUT: ${line}`);
                    });
                  }
                  if (result.stderr.trim()) {
                    result.stderr.trim().split("\n").forEach((line) => {
                      hostLogger.warn(`STDERR: ${line}`);
                    });
                  }
                }

                return { host, result, success: result.success };
              } catch (error) {
                const errorMessage = error instanceof Error
                  ? error.message
                  : String(error);

                if (hostLogger) {
                  hostLogger.error(`Command failed: ${errorMessage}`);
                }

                return {
                  host,
                  result: {
                    stdout: "",
                    stderr: errorMessage,
                    success: false,
                    code: null,
                  },
                  success: false,
                  error: errorMessage,
                };
              } finally {
                if (timerId !== undefined) clearTimeout(timerId);
              }
            });

            const results = await Promise.all(execPromises);
            executionResults.push(...results);
          } else {
            log.status("Executing command sequentially...", "exec");

            // Execute sequentially
            for (const ssh of sshManagers!) {
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
                          `Command timed out after ${options.timeout} seconds`,
                        ),
                      ),
                    options.timeout * 1000,
                  );
                });

                // Race the command execution against the timeout
                const result = await Promise.race([
                  ssh.executeCommand(command),
                  timeoutPromise,
                ]) as Awaited<ReturnType<typeof ssh.executeCommand>>;

                if (hostLogger) {
                  if (result.success) {
                    hostLogger.success(
                      `Command completed (exit code: ${result.code})`,
                    );
                  } else {
                    hostLogger.error(
                      `Command failed (exit code: ${result.code})`,
                    );
                  }

                  if (result.stdout.trim()) {
                    result.stdout.trim().split("\n").forEach((line) => {
                      hostLogger.info(`STDOUT: ${line}`);
                    });
                  }
                  if (result.stderr.trim()) {
                    result.stderr.trim().split("\n").forEach((line) => {
                      hostLogger.warn(`STDERR: ${line}`);
                    });
                  }
                }

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
              } finally {
                if (timerId !== undefined) clearTimeout(timerId);
              }
            }
          }

          // Summary
          const successful = executionResults.filter((r) => r.success);
          const failed = executionResults.filter((r) => !r.success);

          log.info("Execution Summary:", "exec");
          log.success(
            `Successful: ${successful.length} host(s) - ${
              successful.map((r) => r.host).join(", ")
            }`,
            "exec",
          );

          if (failed.length > 0) {
            log.error(
              `Failed: ${failed.length} host(s) - ${
                failed.map((r) => r.host).join(", ")
              }`,
              "exec",
            );
          }

          // Overall success/failure
          if (failed.length > 0 && !options.continueOnError) {
            log.error("Command execution failed on some hosts", "exec");

            // Log failures to audit before exiting
            if (auditLogger) {
              await auditLogger.logCustomCommand(
                command,
                "failed",
                `Command execution failed on ${failed.length} host(s): ${
                  failed.map((r) => r.host).join(", ")
                }`,
              );
            }

            Deno.exit(1);
          } else if (failed.length === 0) {
            log.success("Command executed successfully on all hosts", "exec");
          } else {
            log.warn(
              `Command completed with some failures (${failed.length}/${executionResults.length} hosts failed)`,
              "exec",
            );
          }

          // Log completion to connected servers
          if (auditLogger) {
            await auditLogger.logCustomCommand(
              command,
              failed.length === 0 ? "success" : "failed",
              failed.length === 0
                ? `Command executed successfully on all ${successful.length} host(s)`
                : `Command completed with ${failed.length} failure(s) out of ${executionResults.length} host(s)`,
            );

            log.info(
              `Audit trail updated on ${targetHosts.length} server(s): ${
                targetHosts.join(", ")
              }`,
              "audit",
            );
          }
        });

        // Clean up execution SSH connections before audit logging
        if (sshManagers) {
          sshManagers.forEach((ssh) => {
            try {
              ssh.dispose();
            } catch (error) {
              log.debug(`Failed to dispose SSH connection: ${error}`, "ssh");
            }
          });
          sshManagers = undefined; // Prevent double disposal in finally
        }
      });

      // Final success message
      console.log();
      log.success("Remote command execution completed!");
    } catch (error) {
      const errorMessage = error instanceof Error
        ? error.message
        : String(error);
      console.log();
      log.error("❌ Command execution failed:", "exec");
      log.error(errorMessage, "exec");

      // Log failure to audit if possible
      if (auditLogger) {
        await auditLogger.logCustomCommand(
          command,
          "failed",
          `Command execution failed: ${errorMessage}`,
        );
      }

      Deno.exit(1);
    } finally {
      // Clean up any remaining SSH connections
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
