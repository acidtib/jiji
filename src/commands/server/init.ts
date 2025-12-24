import { Command } from "@cliffy/command";
import { setupCommandContext } from "../../utils/command_helpers.ts";
import { createServerAuditLogger } from "../../utils/audit.ts";
import { log } from "../../utils/logger.ts";
import { handleCommandError } from "../../utils/error_handler.ts";
import { Configuration } from "../../lib/configuration.ts";
import type { GlobalOptions } from "../../types.ts";
import { setupNetwork } from "../../lib/network/setup.ts";

export const initCommand = new Command()
  .description("Initialize servers")
  .action(async (options) => {
    const globalOptions = options as unknown as GlobalOptions;
    let ctx: Awaited<ReturnType<typeof setupCommandContext>> | undefined;

    try {
      log.section("Server Initialization:");

      // Load configuration first (before SSH setup)
      const config = await Configuration.load(
        globalOptions.environment,
        globalOptions.configFile,
      );

      const configPath = config.configPath || "unknown";
      const allHosts = config.getAllServerHosts();

      log.say(`Configuration loaded from: ${configPath}`, 1);
      log.say(`Container engine: ${config.builder.engine}`, 1);
      log.say(
        `Found ${allHosts.length} remote host(s): ${allHosts.join(", ")}`,
        1,
      );

      // Now setup SSH connections
      ctx = await setupCommandContext(globalOptions);
      const { sshManagers, targetHosts } = ctx;

      // Show connection status for each host
      console.log(""); // Empty line
      for (const ssh of sshManagers) {
        log.remote(ssh.getHost(), ": Connected", { indent: 1 });
      }

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

      // Engine installation section
      log.section("Installing Container Engine:");

      const engine = config.builder.engine;

      for (const ssh of sshManagers) {
        const { EngineInstaller } = await import("../../utils/engine.ts");
        const installer = new EngineInstaller(ssh);

        await log.hostBlock(ssh.getHost(), async () => {
          // Check if installed
          const installed = await installer.isEngineInstalled(engine);
          if (!installed) {
            log.say(`├── Installing ${engine}`, 2);
            const result = await installer.installEngine(engine);

            if (!result.success) {
              throw new Error(result.error || `Failed to install ${engine}`);
            }
            log.say(`└── ${engine} installed successfully`, 2);
          } else {
            const version = await installer.getEngineVersion(engine);
            log.say(
              `└── ${engine} already installed${
                version ? ` (${version})` : ""
              }`,
              2,
            );
          }
        }, { indent: 1 });

        // Audit log
        const hostSsh = sshManagers.find((s) => s.getHost() === ssh.getHost());
        if (hostSsh) {
          const auditLogger = createServerAuditLogger(hostSsh, config.project);
          await auditLogger.logEngineInstall(
            engine,
            "success",
            "Installation successful",
          );
        }
      }

      // Network setup (if enabled)
      if (config.network.enabled) {
        await setupNetwork(config, sshManagers);
      }

      // Log successful initialization completion to connected servers
      await auditLogger.logInitSuccess(
        targetHosts,
        config.builder.engine,
      );

      log.section("Initialization Summary:");
      log.say(`- Servers initialized: ${targetHosts.length}`, 1);
      log.say(`- Container engine: ${config.builder.engine}`, 1);
      if (config.network.enabled) {
        log.say(`- Private network: Enabled`, 1);
      }
      console.log();
      log.say(`Initialization completed successfully`);
    } catch (error) {
      await handleCommandError(error, {
        operation: "Initialization",
        component: "init",
        sshManagers: ctx?.sshManagers,
        projectName: ctx?.config?.project,
        targetHosts: ctx?.targetHosts,
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
