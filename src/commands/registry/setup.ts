import { Command } from "@cliffy/command";
import { log } from "../../utils/logger.ts";
import { loadConfig } from "../../utils/config.ts";
import { RegistryService } from "../../lib/registry_service.ts";
import { handleRegistryError } from "../../utils/error_handling.ts";
import type { RegistryCredentials } from "../../types/registry.ts";

export const setupCommand = new Command()
  .description(
    "Setup local registry or log in to remote registry locally and remotely",
  )
  .option("-L, --skip-local", "Skip local setup")
  .option("-R, --skip-remote", "Skip remote setup")
  .action(async (options) => {
    log.info("Setting up registry...", "registry:setup");

    try {
      // Load configuration to get registry settings
      const { config } = await loadConfig();
      const registryConfig = config.builder.registry;

      // Use registry from config
      const registry = registryConfig.getRegistryUrl();

      log.debug(`Registry: ${registry}`, "registry:setup");
      log.debug(`Registry type: ${registryConfig.type}`, "registry:setup");

      // Initialize registry service
      const registryService = new RegistryService();
      await registryService.initialize();

      // Setup local registry unless skip-local is specified
      if (!options.skipLocal) {
        if (registryConfig.type === "local") {
          // Setup local registry
          const setupOptions = {
            type: "local" as const,
            port: registryConfig.port,
          };

          await registryService.setupRegistry(registry, setupOptions);
        } else {
          // For remote registries during local setup, authenticate
          let credentials: RegistryCredentials | undefined;
          if (registryConfig.username && registryConfig.password) {
            credentials = {
              username: registryConfig.username,
              password: registryConfig.password,
            };
          }

          await registryService.authenticate(registry, credentials);
        }
        log.info("Local registry setup completed", "registry:setup");
      } else {
        log.debug("Skipped local registry setup", "registry:setup");
      }

      // Setup remote registry unless skip-remote is specified
      if (!options.skipRemote) {
        if (registryConfig.type === "remote") {
          // Setup remote registry
          let credentials: RegistryCredentials | undefined;
          if (registryConfig.username && registryConfig.password) {
            credentials = {
              username: registryConfig.username,
              password: registryConfig.password,
            };
          }

          const setupOptions = {
            type: "remote" as const,
            credentials,
          };

          await registryService.setupRegistry(registry, setupOptions);
        } else {
          // Local registry doesn't need remote setup
          log.debug(
            "Registry is local, no remote setup needed",
            "registry:setup",
          );
        }
        log.info("Remote registry setup completed", "registry:setup");
      } else {
        log.debug("Skipped remote registry setup", "registry:setup");
      }

      log.info("Registry setup completed successfully", "registry:setup");
    } catch (error) {
      handleRegistryError(error, "setup");
    }
  });
