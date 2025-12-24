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
    try {
      log.section("Registry Setup:");

      // Load configuration to get registry settings
      const { config } = await loadConfig();
      const registryConfig = config.builder.registry;

      // Use registry from config
      const registry = registryConfig.getRegistryUrl();

      log.say(`- Registry: ${registry}`, 1);
      log.say(`- Registry type: ${registryConfig.type}`, 1);

      // Initialize registry service
      const registryService = new RegistryService();
      await registryService.initialize();

      log.section("Setting Up Registry:");

      // Setup local registry unless skip-local is specified
      if (!options.skipLocal) {
        log.say("- Performing local setup", 1);
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
        log.say("- Local registry setup completed", 1);
      } else {
        log.say("- Skipped local registry setup", 1);
      }

      // Setup remote registry unless skip-remote is specified
      if (!options.skipRemote) {
        log.say("- Performing remote setup", 1);
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
          log.say("  Registry is local, no remote setup needed", 2);
        }
        log.say("- Remote registry setup completed", 1);
      } else {
        log.say("- Skipped remote registry setup", 1);
      }

      log.success("\nRegistry setup completed successfully", 0);
    } catch (error) {
      handleRegistryError(error, "setup");
    }
  });
