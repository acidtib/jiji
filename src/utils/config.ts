import { Configuration, ConfigurationError } from "../lib/configuration.ts";
import { ensureDir } from "@std/fs";

const TEMPLATE_PATH = import.meta.dirname + "/../jiji.yml";
const CONFIG_DIR = ".jiji";
const DEFAULT_CONFIG_FILE = "deploy.yml";

/**
 * Configuration load result interface
 */
export interface ConfigLoadResult {
  config: Configuration;
  configPath: string;
}

/**
 * Loads and parses the jiji configuration file using the new configuration system
 */
export async function loadConfig(
  configPath?: string,
  environment?: string,
): Promise<ConfigLoadResult> {
  try {
    const config = await Configuration.load(environment, configPath);
    return {
      config,
      configPath: config.configPath || "",
    };
  } catch (error) {
    if (error instanceof ConfigurationError) {
      throw new Error(error.message);
    }
    throw error;
  }
}

/**
 * Gets the container engine command based on the configuration
 */
export function getEngineCommand(config: Configuration): string {
  return config.engine;
}

/**
 * Checks if a config file exists at the specified path
 */
export async function configFileExists(path: string): Promise<boolean> {
  try {
    await Deno.stat(path);
    return true;
  } catch (error) {
    if (error instanceof Deno.errors.NotFound) {
      return false;
    }
    throw new Error(
      `Error checking config file existence: ${
        error instanceof Error ? error.message : String(error)
      }`,
    );
  }
}

/**
 * Reads the config template file
 */
export async function readConfigTemplate(): Promise<string> {
  try {
    return await Deno.readTextFile(TEMPLATE_PATH);
  } catch (error) {
    throw new Error(
      `Failed to read config template: ${
        error instanceof Error ? error.message : String(error)
      }`,
    );
  }
}

/**
 * Builds the config file path based on environment
 */
export function buildConfigPath(environment?: string): string {
  const fileName = environment
    ? `deploy.${environment}.yml`
    : DEFAULT_CONFIG_FILE;
  return `${CONFIG_DIR}/${fileName}`;
}

/**
 * Creates a config file with the given content
 */
export async function createConfigFile(
  configPath: string,
  template: string,
): Promise<void> {
  try {
    await ensureDir(CONFIG_DIR);
    await Deno.writeTextFile(configPath, template);
  } catch (error) {
    throw new Error(
      `Failed to create config file: ${
        error instanceof Error ? error.message : String(error)
      }`,
    );
  }
}

/**
 * Checks if the specified container engine is available on the system
 */
export async function checkEngineAvailability(
  engine: string,
): Promise<boolean> {
  try {
    const command = new Deno.Command(engine, {
      args: ["--version"],
      stdout: "piped",
      stderr: "piped",
    });

    const { success } = await command.output();
    return success;
  } catch {
    return false;
  }
}

/**
 * Validates a configuration file
 */
export async function validateConfigFile(configPath: string): Promise<void> {
  try {
    const validationResult = await Configuration.validateFile(configPath);

    if (!validationResult.valid) {
      const errorMessages = validationResult.errors
        .map((err) => `${err.path}: ${err.message}`)
        .join("\n");

      throw new Error(`Configuration validation failed:\n${errorMessages}`);
    }

    // Log warnings if any
    if (validationResult.warnings.length > 0) {
      const warningMessages = validationResult.warnings
        .map((warn) => `${warn.path}: ${warn.message}`)
        .join("\n");

      console.warn(`Configuration warnings:\n${warningMessages}`);
    }
  } catch (error) {
    if (error instanceof ConfigurationError) {
      throw new Error(error.message);
    }
    throw error;
  }
}

/**
 * Gets all available configuration files
 */
export async function getAvailableConfigs(
  searchPath?: string,
): Promise<string[]> {
  return await Configuration.getAvailableConfigs(searchPath);
}

/**
 * Creates a configuration with sensible defaults
 */
export function createDefaultConfig(): Configuration {
  return Configuration.withDefaults();
}

/**
 * Filter hosts based on patterns with wildcard support
 * @param allHosts - All available hosts
 * @param hostPatterns - Comma-separated host patterns (supports wildcards with *)
 * @returns Array of matching hosts
 */
export function filterHostsByPatterns(
  allHosts: string[],
  hostPatterns: string,
): string[] {
  const requestedHosts = hostPatterns.split(",").map((h) => h.trim());
  const matchingHosts: string[] = [];

  for (const pattern of requestedHosts) {
    if (pattern.includes("*")) {
      // Simple wildcard matching
      const regex = new RegExp("^" + pattern.replace(/\*/g, ".*") + "$");
      matchingHosts.push(...allHosts.filter((host) => regex.test(host)));
    } else {
      // Exact match
      if (allHosts.includes(pattern)) {
        matchingHosts.push(pattern);
      }
    }
  }

  // Remove duplicates
  return [...new Set(matchingHosts)];
}
