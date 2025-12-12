import { parse } from "@std/yaml";
import { ensureDir, exists } from "@std/fs";
import { dirname, join } from "@std/path";
import type { ConfigLoadResult, JijiConfig } from "../types.ts";

const TEMPLATE_PATH = import.meta.dirname + "/../jiji.yml";
const CONFIG_DIR = "config";
const DEFAULT_CONFIG_FILE = "jiji.yml";

/**
 * Searches for jiji.yml config file starting from current directory
 * and moving up the directory tree
 */
async function findConfigFile(
  startPath: string = Deno.cwd(),
): Promise<string | null> {
  const configFilenames = [
    "config/jiji.yml",
    "config/jiji.yaml",
    "jiji.yml",
    "jiji.yaml",
    ".jiji.yml",
    ".jiji.yaml",
  ];
  let currentPath = startPath;

  while (true) {
    for (const filename of configFilenames) {
      const configPath = join(currentPath, filename);
      if (await exists(configPath)) {
        return configPath;
      }
    }

    const parentPath = dirname(currentPath);
    if (parentPath === currentPath) {
      // Reached filesystem root
      break;
    }
    currentPath = parentPath;
  }

  return null;
}

/**
 * Validates the configuration object structure
 */
function validateConfig(config: unknown): JijiConfig {
  if (!config || typeof config !== "object") {
    throw new Error("Invalid configuration: must be an object");
  }

  const cfg = config as Record<string, unknown>;

  // Validate engine
  if (!cfg.engine) {
    throw new Error("Invalid configuration: 'engine' is required");
  }

  if (typeof cfg.engine !== "string") {
    throw new Error("Invalid configuration: 'engine' must be a string");
  }

  if (!["podman", "docker"].includes(cfg.engine)) {
    throw new Error(
      "Invalid configuration: 'engine' must be either 'podman' or 'docker'",
    );
  }

  // Validate services
  if (!cfg.services) {
    throw new Error("Invalid configuration: 'services' is required");
  }

  if (typeof cfg.services !== "object" || Array.isArray(cfg.services)) {
    throw new Error("Invalid configuration: 'services' must be an object");
  }

  // Validate SSH configuration if present
  if (cfg.ssh) {
    if (typeof cfg.ssh !== "object" || Array.isArray(cfg.ssh)) {
      throw new Error("Invalid configuration: 'ssh' must be an object");
    }

    const sshCfg = cfg.ssh as Record<string, unknown>;

    if (!sshCfg.user || typeof sshCfg.user !== "string") {
      throw new Error(
        "Invalid configuration: 'ssh.user' is required and must be a string",
      );
    }

    if (sshCfg.port !== undefined) {
      if (
        typeof sshCfg.port !== "number" || sshCfg.port <= 0 ||
        sshCfg.port > 65535
      ) {
        throw new Error(
          "Invalid configuration: 'ssh.port' must be a valid port number (1-65535)",
        );
      }
    }
  }

  return cfg as unknown as JijiConfig;
}

/**
 * Loads and parses the jiji.yml configuration file
 */
export async function loadConfig(
  configPath?: string,
): Promise<ConfigLoadResult> {
  let actualConfigPath: string;

  if (configPath) {
    // Use provided path
    if (!await exists(configPath)) {
      throw new Error(`Configuration file not found: ${configPath}`);
    }
    actualConfigPath = configPath;
  } else {
    // Search for config file
    const foundPath = await findConfigFile();
    if (!foundPath) {
      throw new Error(
        "No jiji configuration file found. " +
          "Please create a config/jiji.yml file or specify a config file path.",
      );
    }
    actualConfigPath = foundPath;
  }

  try {
    // Read and parse the YAML file
    const yamlContent = await Deno.readTextFile(actualConfigPath);
    const parsedConfig = parse(yamlContent);

    // Validate the configuration
    const config = validateConfig(parsedConfig);

    return {
      config,
      configPath: actualConfigPath,
    };
  } catch (error) {
    if (error instanceof Error) {
      throw new Error(
        `Failed to load configuration from ${actualConfigPath}: ${error.message}`,
      );
    }
    throw error;
  }
}

/**
 * Gets the container engine command based on the configuration
 */
export function getEngineCommand(config: JijiConfig): string {
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
    ? `jiji.${environment}.yml`
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
