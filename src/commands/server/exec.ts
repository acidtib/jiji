import { Command } from "@cliffy/command";
import { loadConfig } from "../../utils/config.ts";
import {
  createSSHConfigFromJiji,
  createSSHManagers,
  filterConnectedHosts,
  testConnections,
  validateSSHSetup,
} from "../../utils/ssh.ts";
import { createServerAuditLogger } from "../../utils/audit.ts";
import type { Configuration } from "../../lib/configuration.ts";

export const execCommand = new Command()
  .description("Execute a custom command on remote hosts")
  .arguments("<command:string>")
  .option("-c, --config <path:string>", "Path to jiji.yml config file")
  .option("--ssh-user <username:string>", "SSH username for remote hosts")
  .option("--ssh-key <path:string>", "Path to SSH private key")
  .option("--ssh-port <port:number>", "SSH port (default: 22)")
  .option(
    "-h, --hosts <hosts:string>",
    "Comma-separated list of specific hosts to target (default: all hosts)",
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
      console.log(`🚀 Executing command: ${command}`);

      // Load and parse the configuration
      const configResult = await loadConfig(options.config);
      config = configResult.config;
      const configPath = configResult.configPath;
      console.log(`Configuration loaded from: ${configPath}`);

      // Collect all unique hosts from services
      const allHosts = new Set<string>();
      for (const [, service] of config.services) {
        if (service.hosts && service.hosts.length > 0) {
          service.hosts.forEach((host: string) => allHosts.add(host));
        }
      }

      let uniqueHosts = Array.from(allHosts);

      // Filter hosts if specific hosts are requested
      if (options.hosts) {
        const requestedHosts = options.hosts.split(",").map((h) => h.trim());
        const validHosts = requestedHosts.filter((host) =>
          uniqueHosts.includes(host)
        );
        const invalidHosts = requestedHosts.filter((host) =>
          !uniqueHosts.includes(host)
        );

        if (invalidHosts.length > 0) {
          console.warn(
            `⚠️  Invalid hosts specified (not in config): ${
              invalidHosts.join(", ")
            }`,
          );
        }

        if (validHosts.length === 0) {
          console.error("❌ No valid hosts specified");
          Deno.exit(1);
        }

        uniqueHosts = validHosts;
        console.log(`🎯 Targeting specific hosts: ${uniqueHosts.join(", ")}`);
      }

      if (uniqueHosts.length === 0) {
        console.error(`❌ No hosts found in configuration at: ${configPath}`);
        console.error(
          `Please update your jiji config to include hosts for services.`,
        );
        Deno.exit(1);
      }

      console.log(
        `📡 Found ${uniqueHosts.length} host(s): ${uniqueHosts.join(", ")}`,
      );

      // Validate SSH setup
      const sshValidation = await validateSSHSetup();
      if (!sshValidation.valid) {
        console.error(`❌ SSH setup validation failed:`);
        console.error(`   ${sshValidation.message}`);
        console.error(
          `   Please run 'ssh-agent' and 'ssh-add' before continuing.`,
        );
        Deno.exit(1);
      }

      // Get SSH configuration from config file and command line options
      const baseSshConfig = createSSHConfigFromJiji({
        user: config.ssh.user,
        port: config.ssh.port,
      });
      const sshConfig = {
        username: options.sshUser || baseSshConfig.username,
        port: options.sshPort || baseSshConfig.port,
        useAgent: true,
      };

      // Create SSH managers for all hosts and test connections
      sshManagers = createSSHManagers(uniqueHosts, sshConfig);
      const connectionTests = await testConnections(sshManagers);

      const { connectedManagers, connectedHosts, failedHosts } =
        filterConnectedHosts(sshManagers, connectionTests);

      if (connectedHosts.length === 0) {
        console.error("❌ No hosts are reachable. Cannot execute command.");
        Deno.exit(1);
      }

      if (failedHosts.length > 0) {
        console.log(`\n⚠️  Unreachable hosts: ${failedHosts.join(", ")}`);
        if (!options.continueOnError) {
          console.error(
            "❌ Stopping due to unreachable hosts (use --continue-on-error to override)",
          );
          Deno.exit(1);
        }
      }

      console.log(
        `Executing on ${connectedHosts.length} reachable host(s): ${
          connectedHosts.join(", ")
        }\n`,
      );

      // Use only connected SSH managers
      sshManagers = connectedManagers;
      targetHosts = connectedHosts;

      // Create audit logger for connected servers
      auditLogger = createServerAuditLogger(sshManagers);

      // Execute the command on all hosts
      const executionResults = [];

      if (options.parallel) {
        console.log("🔄 Executing command in parallel...\n");

        // Execute in parallel
        const execPromises = sshManagers.map(async (ssh) => {
          const host = ssh.getHost();
          console.log(`[${host}] Starting execution...`);

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

            console.log(
              `[${host}] Command completed (exit code: ${result.code})`,
            );

            if (result.stdout.trim()) {
              console.log(`[${host}] STDOUT:\n${result.stdout.trim()}\n`);
            }
            if (result.stderr.trim()) {
              console.log(`[${host}] STDERR:\n${result.stderr.trim()}\n`);
            }

            return { host, result, success: result.success };
          } catch (error) {
            const errorMessage = error instanceof Error
              ? error.message
              : String(error);
            console.log(`[${host}] ❌ Command failed: ${errorMessage}\n`);
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
        console.log("🔄 Executing command sequentially...\n");

        // Execute sequentially
        for (const ssh of sshManagers) {
          const host = ssh.getHost();
          console.log(`[${host}] Starting execution...`);

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

            console.log(
              `[${host}] Command completed (exit code: ${result.code})`,
            );

            if (result.stdout.trim()) {
              console.log(`[${host}] STDOUT:\n${result.stdout.trim()}\n`);
            }
            if (result.stderr.trim()) {
              console.log(`[${host}] STDERR:\n${result.stderr.trim()}\n`);
            }

            executionResults.push({ host, result, success: result.success });

            // Stop on first failure if continue-on-error is false
            if (!result.success && !options.continueOnError) {
              console.error(`❌ Command failed on ${host}, stopping execution`);
              break;
            }
          } catch (error) {
            const errorMessage = error instanceof Error
              ? error.message
              : String(error);
            console.log(`[${host}] ❌ Command failed: ${errorMessage}\n`);

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
              console.error(`❌ Command failed on ${host}, stopping execution`);
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

      console.log("📊 Execution Summary:");
      console.log(
        `   Successful: ${successful.length} host(s) - ${
          successful.map((r) => r.host).join(", ")
        }`,
      );

      if (failed.length > 0) {
        console.log(
          `   ❌ Failed: ${failed.length} host(s) - ${
            failed.map((r) => r.host).join(", ")
          }`,
        );
      }

      // Overall success/failure
      if (failed.length > 0 && !options.continueOnError) {
        console.log(`\n❌ Command execution failed on some hosts`);

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
        console.log(`\nCommand executed successfully on all hosts`);
      } else {
        console.log(
          `\n⚠️  Command completed with some failures (${failed.length}/${executionResults.length} hosts failed)`,
        );
      }

      // Log successful completion to connected servers
      if (auditLogger) {
        await auditLogger.logCustomCommand(
          command,
          failed.length === 0 ? "success" : "failed",
          failed.length === 0
            ? `Command executed successfully on all ${successful.length} host(s)`
            : `Command completed with ${failed.length} failure(s) out of ${executionResults.length} host(s)`,
        );

        console.log(
          `📋 Audit trail updated on ${targetHosts.length} server(s): ${
            targetHosts.join(", ")
          }`,
        );
      }

      // Clean up execution SSH connections before audit logging
      if (sshManagers) {
        sshManagers.forEach((ssh) => {
          try {
            ssh.dispose();
          } catch (error) {
            console.debug(`Failed to dispose SSH connection: ${error}`);
          }
        });
        sshManagers = undefined; // Prevent double disposal in finally
      }
    } catch (error) {
      const errorMessage = error instanceof Error
        ? error.message
        : String(error);
      console.error("❌ Command execution failed:");
      console.error(errorMessage);

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
            console.debug(`Failed to dispose SSH connection: ${error}`);
          }
        });
      }
    }
  });
