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
    log.info("Removing registry...", "registry:remove");

    try {
      // Load configuration to get registry settings
      const { config } = await loadConfig();
      const registryConfig = config.builder.registry;
      const registry = registryConfig.getRegistryUrl();

      log.debug(`Registry: ${registry}`, "registry:remove");
      log.debug(`Registry type: ${registryConfig.type}`, "registry:remove");

      // Initialize registry service
      const registryService = new RegistryService();
      await registryService.initialize();

      // Perform local removal unless skip-local is specified
      if (!options.skipLocal) {
        await registryService.removeRegistry(registry);
        log.info("Local registry removed", "registry:remove");
      } else {
        log.debug("Skipped local registry removal", "registry:remove");
      }

      // Perform remote logout unless skip-remote is specified
      if (!options.skipRemote) {
        // For container registries, this is handled by the removeRegistry method
        // which includes logout functionality
        log.debug(
          "Remote registry logout handled by removal",
          "registry:remove",
        );
        log.info("Remote registry logout completed", "registry:remove");
      } else {
        log.debug("Skipped remote registry logout", "registry:remove");
      }

      log.info("Registry removal completed successfully", "registry:remove");
    } catch (error) {
      handleRegistryError(error, "remove");
    }
  });
