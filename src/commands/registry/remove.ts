import { Command } from "@cliffy/command";
import { log } from "../../utils/logger.ts";
import { RegistryManager } from "../../utils/registry_manager.ts";
import { loadConfig } from "../../utils/config.ts";
import { getRegistryConfigManager } from "../../utils/registry_config.ts";

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

      // Perform local removal unless skip-local is specified
      if (!options.skipLocal) {
        await removeLocalRegistry(registry);
        log.info("✓ Local registry removed", "registry:remove");
      } else {
        log.debug("Skipped local registry removal", "registry:remove");
      }

      // Perform remote logout unless skip-remote is specified
      if (!options.skipRemote) {
        await logoutRemoteRegistry(registry);
        log.info("✓ Remote registry logout completed", "registry:remove");
      } else {
        log.debug("Skipped remote registry logout", "registry:remove");
      }

      log.info("Registry removal completed successfully", "registry:remove");
    } catch (error) {
      log.error(
        `Registry removal failed: ${
          error instanceof Error ? error.message : String(error)
        }`,
        "registry:remove",
      );
      Deno.exit(1);
    }
  });

async function removeLocalRegistry(registry: string): Promise<void> {
  log.debug("Removing local registry configuration...", "registry:remove");

  try {
    // Check if this is a local registry (localhost)
    if (registry.includes("localhost") || registry.includes("127.0.0.1")) {
      // Get configuration to determine container engine
      const { config } = await loadConfig();
      const engine = config.builder?.engine || "docker";

      // Extract port from registry URL
      const portMatch = registry.match(/:(\d+)/);
      const port = portMatch ? parseInt(portMatch[1]) : 6767;

      // Use RegistryManager to remove local registry
      const registryManager = new RegistryManager(engine, port);
      await registryManager.remove();

      log.debug("Local registry container removed", "registry:remove");

      // Remove from configuration
      const configManager = getRegistryConfigManager();
      await configManager.removeRegistry(registry);
      log.debug("Removed local registry from configuration", "registry:remove");
    } else {
      // For remote registries, perform logout
      const { config } = await loadConfig();
      const engine = config.builder?.engine || "docker";

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

      log.debug("Remote registry credentials removed", "registry:remove");

      // Remove from configuration
      const configManager = getRegistryConfigManager();
      await configManager.removeRegistry(registry);
      log.debug(
        "Removed remote registry from configuration",
        "registry:remove",
      );
    }
  } catch (error) {
    throw new Error(
      `Failed to remove local registry: ${
        error instanceof Error ? error.message : String(error)
      }`,
    );
  }
}

async function logoutRemoteRegistry(registry: string): Promise<void> {
  log.debug("Logging out from remote registry...", "registry:remove");

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
      // If already not logged in, that's actually success for our logout purposes
      if (error.toLowerCase().includes("not logged in")) {
        log.debug("Already not logged into registry", "registry:remove");
        // Still remove from configuration even if not logged in
        const configManager = getRegistryConfigManager();
        await configManager.removeRegistry(registry);
        log.debug("Removed registry from configuration", "registry:remove");
        return;
      }
      throw new Error(`Registry logout failed: ${error}`);
    }

    log.debug("Remote registry logout successful", "registry:remove");

    // Remove from configuration
    const configManager = getRegistryConfigManager();
    await configManager.removeRegistry(registry);
    log.debug("Removed registry from configuration", "registry:remove");
  } catch (error) {
    throw new Error(
      `Failed to logout from remote registry: ${
        error instanceof Error ? error.message : String(error)
      }`,
    );
  }
}
