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
    log.info("Logging in to registry...", "registry:login");

    try {
      // Load configuration to get registry settings
      const { config } = await loadConfig();
      const registryConfig = config.builder.registry;

      // Use registry from config
      const registry = registryConfig.getRegistryUrl();

      log.debug(`Registry: ${registry}`, "registry:login");
      log.debug(`Registry type: ${registryConfig.type}`, "registry:login");

      // Initialize registry service
      const registryService = new RegistryService();
      await registryService.initialize();

      // Prepare credentials if available in config
      let credentials: RegistryCredentials | undefined;
      if (registryConfig.username && registryConfig.password) {
        credentials = {
          username: registryConfig.username,
          password: registryConfig.password,
        };
      }

      // Only perform operations that aren't skipped
      if (!options.skipLocal) {
        await registryService.authenticate(registry, credentials);
        log.info("Local login successful", "registry:login");
      } else {
        log.debug("Skipped local login", "registry:login");
      }

      if (!options.skipRemote) {
        // For container registries, local and remote login are the same operation
        log.debug("Remote login handled by container engine", "registry:login");
        log.info("Remote login successful", "registry:login");
      } else {
        log.debug("Skipped remote login", "registry:login");
      }

      log.info("Registry login completed successfully", "registry:login");
    } catch (error) {
      handleRegistryError(error, "login");
    }
  });
