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
    try {
      log.section("Registry Logout:");

      // Load configuration to get registry settings
      const { config } = await loadConfig();
      const registryConfig = config.builder.registry;
      const registry = registryConfig.getRegistryUrl();

      log.say(`- Registry: ${registry}`, 1);
      log.say(`- Registry type: ${registryConfig.type}`, 1);

      // Initialize authenticator with container engine
      const engine = config.builder?.engine || "docker";
      const authenticator = new RegistryAuthenticator(engine);

      log.section("Logging Out:");

      // Only perform operations that aren't skipped
      if (!options.skipLocal) {
        log.say("- Performing local logout", 1);
        await authenticator.logout(registry);
        log.say("- Local logout successful", 1);
      } else {
        log.say("- Skipped local logout", 1);
      }

      if (!options.skipRemote) {
        log.say("- Performing remote logout", 1);
        // For container registries, local and remote logout are the same operation
        log.say("- Remote logout successful", 1);
      } else {
        log.say("- Skipped remote logout", 1);
      }

      log.success("\nRegistry logout completed successfully", 0);
    } catch (error) {
      handleRegistryError(error, "logout");
    }
  });
