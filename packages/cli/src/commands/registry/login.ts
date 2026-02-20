import { Command } from "@cliffy/command";
import { log } from "../../utils/logger.ts";
import { RegistryService } from "../../lib/registry_service.ts";
import { handleRegistryError } from "../../utils/error_handling.ts";
import {
  cleanupSSHConnections,
  setupCommandContext,
} from "../../utils/command_helpers.ts";
import type { GlobalOptions } from "../../types.ts";
import type { RegistryCredentials } from "../../types/registry.ts";

export const loginCommand = new Command()
  .description("Log in to registry locally and/or on remote servers")
  .option("-L, --skip-local", "Skip local login")
  .option("-R, --skip-remote", "Skip remote login on servers")
  .action(async (options) => {
    const globalOptions = options as unknown as GlobalOptions;
    let ctx: Awaited<ReturnType<typeof setupCommandContext>> | undefined;

    try {
      log.section("Registry Login:");

      // Setup command context to get config and SSH connections
      ctx = await setupCommandContext(globalOptions);
      const { config, sshManagers } = ctx;

      const registryConfig = config.builder.registry;
      const registry = registryConfig.getRegistryUrl();

      log.say(`- Registry: ${registry}`, 1);
      log.say(`- Registry type: ${registryConfig.type}`, 1);

      const registryService = new RegistryService();
      await registryService.initialize();

      let credentials: RegistryCredentials | undefined;
      if (registryConfig.username && registryConfig.password) {
        credentials = {
          username: registryConfig.username,
          password: registryConfig.password,
        };
      }

      log.section("Authenticating:");

      // Local login
      if (!options.skipLocal) {
        log.say("- Performing local login", 1);
        await registryService.authenticate(registry, credentials);
        log.say("- Local login successful", 1);
      } else {
        log.say("- Skipped local login", 1);
      }

      // Remote login
      if (!options.skipRemote) {
        log.say(
          `- Performing remote login on ${sshManagers.length} server(s)`,
          1,
        );
        const result = await registryService.authenticateOnRemoteServers(
          registry,
          credentials,
          sshManagers,
        );

        if (result.success) {
          log.say("- Remote login successful on all servers", 1);
        } else {
          log.warn("- Remote login failed on some servers:", "registry");
          for (const error of result.errors) {
            log.say(`  • ${error}`, 2);
          }
          throw new Error(
            `Remote login failed on ${result.errors.length} server(s)`,
          );
        }
      } else {
        log.say("- Skipped remote login", 1);
      }

      log.success("\nRegistry login completed successfully", 0);
    } catch (error) {
      handleRegistryError(error, "login");
    } finally {
      if (ctx) {
        await cleanupSSHConnections(ctx.sshManagers);
      }
    }
  });
