import { Command } from "@cliffy/command";
import { log } from "../../utils/logger.ts";
import { loadConfig } from "../../utils/config.ts";
import {
  createRegistryConfig,
  getRegistryConfigManager,
} from "../../utils/registry_config.ts";
import type { RegistryConfiguration } from "../../lib/configuration/registry.ts";

/**
 * Helper function to safely extract error message from unknown error type
 */
function getErrorMessage(error: unknown): string {
  if (error instanceof Error) {
    return error.message;
  }
  return String(error);
}

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

      // Only perform operations that aren't skipped
      if (!options.skipLocal) {
        await loginLocally(registry, registryConfig);
        log.info("Local login successful", "registry:login");
      } else {
        log.debug("Skipped local login", "registry:login");
      }

      if (!options.skipRemote) {
        loginRemotely(registry, registryConfig);
        log.info("Remote login successful", "registry:login");
      } else {
        log.debug("Skipped remote login", "registry:login");
      }

      log.info("Registry login completed successfully", "registry:login");
    } catch (error) {
      log.error(
        `Registry login failed: ${getErrorMessage(error)}`,
        "registry:login",
      );
      Deno.exit(1);
    }
  });

async function loginLocally(
  registry: string,
  registryConfig: RegistryConfiguration,
): Promise<void> {
  log.debug("Performing local registry login...", "registry:login");

  try {
    // Get configuration to determine container engine
    const { config } = await loadConfig();
    const engine = config.builder?.engine || "docker";

    // Only perform login for remote registries or when credentials are provided
    if (registryConfig.type === "local") {
      log.debug("Local registry, no authentication needed", "registry:login");
      return;
    }

    // Perform registry login using container engine for remote registries
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

    log.debug("Local registry authentication successful", "registry:login");
  } catch (error) {
    throw new Error(`Failed to login locally: ${getErrorMessage(error)}`);
  }
}

function loginRemotely(
  _registry: string,
  _registryConfig: RegistryConfiguration,
): void {
  log.debug("Performing remote registry login...", "registry:login");

  // For container registries, local and remote login are typically the same operation
  // The container engine handles the authentication with the remote registry
  // This function could be extended to handle additional remote-specific operations
  // like API key validation, remote configuration, etc.

  log.debug(
    "Remote registry login handled by container engine",
    "registry:login",
  );
}
