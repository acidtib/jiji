import { Command } from "@cliffy/command";
import { log } from "../../utils/logger.ts";
import { RegistryService } from "../../lib/registry_service.ts";
import { handleRegistryError } from "../../utils/error_handling.ts";
import {
  cleanupSSHConnections,
  setupCommandContext,
} from "../../utils/command_helpers.ts";
import type { GlobalOptions } from "../../types.ts";

export const logoutCommand = new Command()
  .description("Log out of registry locally and/or on remote servers")
  .option("-L, --skip-local", "Skip local logout")
  .option("-R, --skip-remote", "Skip remote logout on servers")
  .action(async (options) => {
    const globalOptions = options as unknown as GlobalOptions;
    let ctx: Awaited<ReturnType<typeof setupCommandContext>> | undefined;

    try {
      log.section("Registry Logout:");

      // Setup command context to get config and SSH connections
      ctx = await setupCommandContext(globalOptions);
      const { config, sshManagers } = ctx;

      const registryConfig = config.builder.registry;
      const registry = registryConfig.getRegistryUrl();

      log.say(`- Registry: ${registry}`, 1);
      log.say(`- Registry type: ${registryConfig.type}`, 1);

      // Initialize registry service with container engine
      const engine = config.builder?.engine || "docker";
      const registryService = new RegistryService(engine);
      await registryService.initialize();

      log.section("Logging Out:");

      // Local logout
      if (!options.skipLocal) {
        log.say("- Performing local logout", 1);
        await registryService.logout(registry);
        log.say("- Local logout successful", 1);
      } else {
        log.say("- Skipped local logout", 1);
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

      log.success("\nRegistry logout completed successfully", 0);
    } catch (error) {
      handleRegistryError(error, "logout");
    } finally {
      if (ctx) {
        await cleanupSSHConnections(ctx.sshManagers);
      }
    }
  });
