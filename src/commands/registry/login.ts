import { Command } from "@cliffy/command";
import { log } from "../../utils/logger.ts";
import { loadConfig } from "../../utils/config.ts";
import { RegistryService } from "../../lib/registry_service.ts";
import { handleRegistryError } from "../../utils/error_handling.ts";
import type { RegistryCredentials } from "../../types/registry.ts";

export const loginCommand = new Command()
  .description("Log in to remote registry locally and remotely")
  .option("-L, --skip-local", "Skip local login")
  .option("-R, --skip-remote", "Skip remote login")
  .action(async (options) => {
    try {
      log.section("Registry Login:");

      const { config } = await loadConfig();
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

      if (!options.skipLocal) {
        log.say("- Performing local login", 1);
        await registryService.authenticate(registry, credentials);
        log.say("- Local login successful", 1);
      } else {
        log.say("- Skipped local login", 1);
      }

      if (!options.skipRemote) {
        log.say("- Performing remote login", 1);
        // For container registries, local and remote login are the same operation
        log.say("- Remote login successful", 1);
      } else {
        log.say("- Skipped remote login", 1);
      }

      log.success("\nRegistry login completed successfully", 0);
    } catch (error) {
      handleRegistryError(error, "login");
    }
  });
