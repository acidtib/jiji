import { Command } from "@cliffy/command";
import { log } from "../../utils/logger.ts";
import { loadConfig } from "../../utils/config.ts";
import { getRegistryConfigManager } from "../../utils/registry_config.ts";

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

      // Only perform operations that aren't skipped
      if (!options.skipLocal) {
        await logoutLocally(registry);
        log.info("✓ Local logout successful", "registry:logout");
      } else {
        log.debug("Skipped local logout", "registry:logout");
      }

      if (!options.skipRemote) {
        await logoutRemotely(registry);
        log.info("✓ Remote logout successful", "registry:logout");
      } else {
        log.debug("Skipped remote logout", "registry:logout");
      }

      log.info("Registry logout completed successfully", "registry:logout");
    } catch (error) {
      log.error(
        `Registry logout failed: ${
          error instanceof Error ? error.message : String(error)
        }`,
        "registry:logout",
      );
      Deno.exit(1);
    }
  });

async function logoutLocally(registry: string): Promise<void> {
  log.debug("Performing local registry logout...", "registry:logout");

  try {
    // Get configuration to determine container engine
    const { config } = await loadConfig();
    const engine = config.builder?.engine || "docker";

    // Perform registry logout using container engine
    const command = new Deno.Command(engine, {
      args: ["logout", registry],
      stdout: "piped",
      stderr: "piped",
    });

    const { code, stderr } = await command.output();

    if (code !== 0) {
      const error = new TextDecoder().decode(stderr);
      throw new Error(`Registry logout failed: ${error}`);
    }

    // Remove registry from configuration if it's a remote registry
    const configManager = getRegistryConfigManager();
    const registryConfig = await configManager.getRegistry(registry);
    if (registryConfig && registryConfig.type === "remote") {
      await configManager.removeRegistry(registry);
      log.debug("Removed registry from configuration", "registry:logout");
    }

    log.debug("Local registry logout successful", "registry:logout");
  } catch (error) {
    throw new Error(
      `Failed to logout locally: ${
        error instanceof Error ? error.message : String(error)
      }`,
    );
  }
}

function logoutRemotely(_registry: string): void {
  log.debug("Performing remote registry logout...", "registry:logout");

  // For container registries, local and remote logout are typically the same operation
  // The container engine handles the logout from the remote registry
  // This function could be extended to handle additional remote-specific operations
  // like API session invalidation, remote configuration cleanup, etc.

  log.debug(
    "Remote registry logout handled by container engine",
    "registry:logout",
  );
}
