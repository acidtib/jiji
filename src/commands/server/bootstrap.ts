import { Command } from "@cliffy/command";
import { getEngineCommand, loadConfig } from "../../utils/config.ts";
import {
  createSSHConfigFromJiji,
  createSSHManagers,
  filterConnectedHosts,
  testConnections,
  validateSSHSetup,
} from "../../utils/ssh.ts";
import { installEngineOnHosts } from "../../utils/engine.ts";
import { createServerAuditLogger } from "../../utils/audit.ts";
import type { JijiConfig } from "../../types.ts";

export const bootstrapCommand = new Command()
  .description("Bootstrap servers with curl and Podman or Docker")
  .option("-c, --config <path:string>", "Path to jiji.yml config file")
  .option("--ssh-user <username:string>", "SSH username for remote hosts")
  .option("--ssh-key <path:string>", "Path to SSH private key")
  .option("--ssh-port <port:number>", "SSH port (default: 22)")
  .action(async (options) => {
    let uniqueHosts: string[] = [];
    let config: JijiConfig | undefined;
    let auditLogger: ReturnType<typeof createServerAuditLogger> | undefined;
    let sshManagers: ReturnType<typeof createSSHManagers> | undefined;
    let installResults:
      | Awaited<ReturnType<typeof installEngineOnHosts>>
      | undefined;

    try {
      console.log("Server bootstrap command called!");

      // Load and parse the configuration
      const configResult = await loadConfig(options.config);
      config = configResult.config;
      const configPath = configResult.configPath;
      console.log(`Configuration loaded from: ${configPath}`);

      // Configuration loading will be logged once we have SSH connections
      console.log(`Container engine: ${config.engine}`);

      // Check if the specified engine is available
      const engineCommand = getEngineCommand(config);

      // Collect all unique hosts from services
      const allHosts = new Set<string>();
      for (const service of Object.values(config.services)) {
        if (service.hosts) {
          service.hosts.forEach((host: string) => allHosts.add(host));
        }
      }

      uniqueHosts = Array.from(allHosts);

      if (uniqueHosts.length > 0) {
        console.log(
          `Found ${uniqueHosts.length} remote host(s): ${
            uniqueHosts.join(", ")
          }`,
        );

        // Always install engine on remote hosts
        console.log(`\nInstalling ${engineCommand} on remote hosts...`);

        // Validate SSH setup
        const sshValidation = await validateSSHSetup();
        if (!sshValidation.valid) {
          console.error(`SSH setup validation failed:`);
          console.error(`   ${sshValidation.message}`);
          console.error(
            `   Please run 'ssh-agent' and 'ssh-add' before continuing.`,
          );
          Deno.exit(1);
        }

        // Get SSH configuration from config file and command line options
        const baseSshConfig = createSSHConfigFromJiji(config.ssh);
        const sshConfig = {
          username: options.sshUser || baseSshConfig.username,
          port: options.sshPort || baseSshConfig.port,
          useAgent: true,
        };

        try {
          // Create SSH managers for all hosts and test connections
          sshManagers = createSSHManagers(uniqueHosts, sshConfig);
          const connectionTests = await testConnections(sshManagers);

          const { connectedManagers, connectedHosts, failedHosts } =
            filterConnectedHosts(sshManagers, connectionTests);

          if (connectedHosts.length === 0) {
            console.error(
              "❌ No hosts are reachable. Cannot proceed with bootstrap.",
            );
            Deno.exit(1);
          }

          if (failedHosts.length > 0) {
            console.log(
              `\n⚠️  Skipping unreachable hosts: ${failedHosts.join(", ")}`,
            );
            console.log(
              `✅ Proceeding with ${connectedHosts.length} reachable host(s): ${
                connectedHosts.join(", ")
              }\n`,
            );
          }

          // Use only connected SSH managers
          sshManagers = connectedManagers;

          // Update uniqueHosts to only connected hosts for consistent reporting
          uniqueHosts = connectedHosts;

          // Create audit logger for connected servers only
          auditLogger = createServerAuditLogger(sshManagers);

          // Log bootstrap start to connected servers
          await auditLogger.logBootstrapStart(
            connectedHosts,
            config.engine,
          );

          // Log configuration loading to connected servers
          await auditLogger.logConfigChange(configPath, "loaded");

          installResults = await installEngineOnHosts(
            sshManagers,
            config.engine,
          );

          // Log engine installation results to each respective server
          for (const result of installResults) {
            const hostSsh = sshManagers.find((
              ssh: ReturnType<typeof createSSHManagers>[0],
            ) => ssh.getHost() === result.host);
            if (hostSsh) {
              const hostLogger = createServerAuditLogger(hostSsh);
              await hostLogger.logEngineInstall(
                config.engine,
                result.success ? "success" : "failed",
                result.message ||
                  (result.success ? "Installation successful" : result.error),
              );
            }
          }

          // SSH connections will be cleaned up in finally block
        } catch (error) {
          const errorMessage = error instanceof Error
            ? error.message
            : String(error);
          console.error(`Engine installation failed: ${errorMessage}`);

          // Log engine installation failure
          if (auditLogger && sshManagers) {
            for (const host of uniqueHosts) {
              const hostSsh = sshManagers.find((
                ssh: ReturnType<typeof createSSHManagers>[0],
              ) => ssh.getHost() === host);
              if (hostSsh) {
                const hostLogger = createServerAuditLogger(hostSsh);
                await hostLogger.logEngineInstall(
                  config.engine,
                  "failed",
                  errorMessage,
                );
              }
            }
          }

          console.log(`Continuing with bootstrap process...`);
        }
      } else {
        console.log(`No remote hosts found in configuration at: ${configPath}`);
        console.log(
          `Could not find any hosts. Please update your jiji config to include hosts for services.`,
        );
        Deno.exit(1);
      }

      // Log successful bootstrap completion to connected servers
      if (auditLogger) {
        const auditResults = await auditLogger.logBootstrapSuccess(
          uniqueHosts,
          config.engine,
        );

        // Report successful audit logging (should match connected hosts)
        const successfulHosts = auditResults
          .filter((result) => result.success)
          .map((result) => result.host);

        console.log(
          `\n✅ Bootstrap completed successfully on ${uniqueHosts.length} server(s)`,
        );
        console.log(
          `📋 Audit trail updated on ${successfulHosts.length} server(s): ${
            successfulHosts.join(", ")
          }`,
        );
      }
    } catch (error) {
      const errorMessage = error instanceof Error
        ? error.message
        : String(error);
      console.error("Bootstrap failed:");
      console.error(errorMessage);

      // Log bootstrap failure to servers if possible
      if (auditLogger) {
        const failureResults = await auditLogger.logBootstrapFailure(
          errorMessage,
          uniqueHosts,
          config ? config.engine : undefined,
        );

        // Report which servers received the failure log
        const successfulFailureLogs = failureResults
          .filter((result) => result.success)
          .map((result) => result.host);

        if (successfulFailureLogs.length > 0) {
          console.log(
            `Failure logged to ${successfulFailureLogs.length} server(s): ${
              successfulFailureLogs.join(", ")
            }`,
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
            console.debug(`Failed to dispose SSH connection: ${error}`);
          }
        });
      }
    }
  });
