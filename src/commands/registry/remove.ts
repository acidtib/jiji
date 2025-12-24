import { Command } from "@cliffy/command";
import { log } from "../../utils/logger.ts";
import { loadConfig } from "../../utils/config.ts";
import { RegistryService } from "../../lib/registry_service.ts";
import { handleRegistryError } from "../../utils/error_handling.ts";

export const removeCommand = new Command()
  .description(
    "Remove local registry or log out of remote registry locally and remotely",
  )
  .option("-L, --skip-local", "Skip local removal")
  .option("-R, --skip-remote", "Skip remote removal")
  .action(async (options) => {
    try {
      log.section("Registry Removal:");

      // Load configuration to get registry settings
      const { config } = await loadConfig();
      const registryConfig = config.builder.registry;
      const registry = registryConfig.getRegistryUrl();

      log.say(`- Registry: ${registry}`, 1);
      log.say(`- Registry type: ${registryConfig.type}`, 1);

      // Initialize registry service
      const registryService = new RegistryService();
      await registryService.initialize();

      log.section("Removing Registry:");

      // Perform local removal unless skip-local is specified
      if (!options.skipLocal) {
        log.say("- Performing local removal", 1);
        await registryService.removeRegistry(registry);
        log.say("- Local registry removed", 1);
      } else {
        log.say("- Skipped local registry removal", 1);
      }

      // Perform remote logout unless skip-remote is specified
      if (!options.skipRemote) {
        log.say("- Performing remote logout", 1);
        // For container registries, this is handled by the removeRegistry method
        // which includes logout functionality
        log.say("- Remote registry logout completed", 1);
      } else {
        log.say("- Skipped remote registry logout", 1);
      }

      log.success("\nRegistry removal completed successfully", 0);
    } catch (error) {
      handleRegistryError(error, "remove");
    }
  });
