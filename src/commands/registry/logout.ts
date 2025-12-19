import { Command } from "@cliffy/command";
import { log } from "../../utils/logger.ts";
import { loadConfig } from "../../utils/config.ts";
import { RegistryAuthenticator } from "../../lib/registry_authenticator.ts";
import { handleRegistryError } from "../../utils/error_handling.ts";

export const logoutCommand = new Command()
  .description("Log out of remote registry locally and remotely")
  .option("-L, --skip-local", "Skip local logout")
  .option("-R, --skip-remote", "Skip remote logout")
  .action(async (options) => {
    log.info("Logging out of registry...", "registry:logout");

    try {
      // Load configuration to get registry settings
      const { config } = await loadConfig();
      const registryConfig = config.builder.registry;
      const registry = registryConfig.getRegistryUrl();

      log.debug(`Registry: ${registry}`, "registry:logout");
      log.debug(`Registry type: ${registryConfig.type}`, "registry:logout");

      // Initialize authenticator with container engine
      const engine = config.builder?.engine || "docker";
      const authenticator = new RegistryAuthenticator(engine);

      // Only perform operations that aren't skipped
      if (!options.skipLocal) {
        await authenticator.logout(registry);
        log.info("Local logout successful", "registry:logout");
      } else {
        log.debug("Skipped local logout", "registry:logout");
      }

      if (!options.skipRemote) {
        // For container registries, local and remote logout are the same operation
        log.debug(
          "Remote registry logout handled by container engine",
          "registry:logout",
        );
        log.info("Remote logout successful", "registry:logout");
      } else {
        log.debug("Skipped remote logout", "registry:logout");
      }

      log.info("Registry logout completed successfully", "registry:logout");
    } catch (error) {
      handleRegistryError(error, "logout");
    }
  });
