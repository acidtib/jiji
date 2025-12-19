import { Command } from "@cliffy/command";
import { log } from "../../utils/logger.ts";
import { RegistryManager } from "../../utils/registry_manager.ts";
import { loadConfig } from "../../utils/config.ts";
import {
  createRegistryConfig,
  getRegistryConfigManager,
} from "../../utils/registry_config.ts";
import type { RegistryConfiguration } from "../../lib/configuration/registry.ts";

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

      // Setup local registry unless skip-local is specified
      if (!options.skipLocal) {
        await setupLocalRegistry(registry, registryConfig, options);
        log.info("✓ Local registry setup completed", "registry:setup");
      } else {
        log.debug("Skipped local registry setup", "registry:setup");
      }

      // Setup remote registry unless skip-remote is specified
      if (!options.skipRemote) {
        await setupRemoteRegistry(registry, registryConfig);
        log.info("✓ Remote registry setup completed", "registry:setup");
      } else {
        log.debug("Skipped remote registry setup", "registry:setup");
      }

      log.info("Registry setup completed successfully", "registry:setup");
    } catch (error) {
      log.error(
        `Registry setup failed: ${
          error instanceof Error ? error.message : String(error)
        }`,
        "registry:setup",
      );
      Deno.exit(1);
    }
  });

async function setupLocalRegistry(
  registry: string,
  registryConfig: RegistryConfiguration,
  _options: unknown,
): Promise<void> {
  log.debug("Setting up local registry...", "registry:setup");

  const port = registryConfig.port || 6767;

  try {
    // Get configuration to determine container engine
    const { config } = await loadConfig();
    const engine = config.builder?.engine || "docker";

    // Create registry manager instance
    const registryManager = new RegistryManager(engine, port);

    // Check if registry is already running
    const status = await registryManager.getStatus();
    if (status.running) {
      log.info(
        `Local registry already running on port ${port}`,
        "registry:setup",
      );
    } else {
      // Start the local registry
      await registryManager.start();
    }

    // Save registry configuration
    const configManager = getRegistryConfigManager();
    const registryConfig = createRegistryConfig(registry, {
      type: "local",
      port: port,
      isDefault: !await configManager.getDefaultRegistry(), // Make default if none exists
    });

    await configManager.addRegistry(registryConfig);

    log.debug(
      `Local registry setup completed on port ${port}`,
      "registry:setup",
    );
  } catch (error) {
    throw new Error(
      `Failed to setup local registry: ${
        error instanceof Error ? error.message : String(error)
      }`,
    );
  }
}

async function setupRemoteRegistry(
  registry: string,
  registryConfig: RegistryConfiguration,
): Promise<void> {
  log.debug("Setting up remote registry connection...", "registry:setup");

  try {
    // Only setup remote registry if it's actually a remote registry
    if (registryConfig.type === "local") {
      log.debug("Registry is local, no remote setup needed", "registry:setup");
      return;
    }

    // Get configuration to determine container engine
    const { config } = await loadConfig();
    const engine = config.builder?.engine || "docker";

    // Perform registry login using container engine
    const loginArgs = ["login"];

    if (registryConfig.username) {
      loginArgs.push("--username", registryConfig.username);
    }

    if (registryConfig.password) {
      loginArgs.push("--password-stdin");
    }

    loginArgs.push(registry);

    const command = new Deno.Command(engine, {
      args: loginArgs,
      stdin: "piped",
      stdout: "piped",
      stderr: "piped",
    });

    const process = command.spawn();

    if (registryConfig.password) {
      const writer = process.stdin.getWriter();
      await writer.write(new TextEncoder().encode(registryConfig.password));
      await writer.close();
    }

    const { code, stderr } = await process.output();

    if (code !== 0) {
      const error = new TextDecoder().decode(stderr);
      throw new Error(`Registry login failed: ${error}`);
    }

    // Save registry configuration
    const configManager = getRegistryConfigManager();
    const registryConfigEntry = createRegistryConfig(registry, {
      type: "remote",
      username: registryConfig.username,
      isDefault: !await configManager.getDefaultRegistry(), // Make default if none exists
    });

    await configManager.addRegistry(registryConfigEntry);

    log.debug("Remote registry authentication successful", "registry:setup");
  } catch (error) {
    throw new Error(
      `Failed to setup remote registry: ${
        error instanceof Error ? error.message : String(error)
      }`,
    );
  }
}
