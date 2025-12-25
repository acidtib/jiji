import { Command } from "@cliffy/command";
import { log } from "../../utils/logger.ts";
import { RegistryService } from "../../lib/registry_service.ts";
import { handleRegistryError } from "../../utils/error_handling.ts";
import {
  cleanupSSHConnections,
  setupCommandContext,
} from "../../utils/command_helpers.ts";
import type { GlobalOptions } from "../../types.ts";

export const removeCommand = new Command()
  .description(
    "Remove local registry container and/or logout from registry on remote servers",
  )
  .option("-L, --skip-local", "Skip local registry removal/logout")
  .option("-R, --skip-remote", "Skip remote logout on servers")
  .action(async (options) => {
    const globalOptions = options as unknown as GlobalOptions;
    let ctx: Awaited<ReturnType<typeof setupCommandContext>> | undefined;

    try {
      log.section("Registry Removal:");

      // Setup command context to get config and SSH connections
      ctx = await setupCommandContext(globalOptions);
      const { config, sshManagers } = ctx;

      const registryConfig = config.builder.registry;
      const registry = registryConfig.getRegistryUrl();

      log.say(`- Registry: ${registry}`, 1);
      log.say(`- Registry type: ${registryConfig.type}`, 1);

      // Initialize registry service
      const registryService = new RegistryService();
      await registryService.initialize();

      log.section("Removing Registry:");

      // Local removal
      if (!options.skipLocal) {
        log.say("- Performing local registry removal", 1);
        await registryService.removeRegistry(registry);

        if (registryConfig.type === "local") {
          log.say("- Local registry container stopped and removed", 1);
        } else {
          log.say("- Logged out from remote registry locally", 1);
        }
      } else {
        log.say("- Skipped local registry removal", 1);
      }

      // Remote logout
      if (!options.skipRemote) {
        log.say(
          `- Performing remote logout on ${sshManagers.length} server(s)`,
          1,
        );
        const result = await registryService.logoutFromRemoteServers(
          registry,
          sshManagers,
        );

        if (result.success) {
          log.say("- Remote logout successful on all servers", 1);
        } else {
          log.warn("- Remote logout failed on some servers:", "registry");
          for (const error of result.errors) {
            log.say(`  • ${error}`, 2);
          }
          throw new Error(
            `Remote logout failed on ${result.errors.length} server(s)`,
          );
        }
      } else {
        log.say("- Skipped remote logout", 1);
      }

      log.success("\nRegistry removal completed successfully", 0);
    } catch (error) {
      handleRegistryError(error, "remove");
    } finally {
      if (ctx) {
        await cleanupSSHConnections(ctx.sshManagers);
      }
    }
  });
