import { parse } from "@std/yaml";
import { exists } from "@std/fs";
import { dirname, join } from "@std/path";
import { ConfigurationError } from "./base.ts";

/**
 * Configuration file loader with environment support
 */
export class ConfigurationLoader {
  private static readonly CONFIG_DIR = ".jiji";
  private static readonly CONFIG_EXTENSIONS = [".yml", ".yaml"];
  private static readonly DEFAULT_CONFIG_FILES = [
    "deploy",
  ];

  /**
   * Searches for configuration files in the file system
   */
  static async findConfigFile(
    environment?: string,
    startPath: string = Deno.cwd(),
  ): Promise<string | null> {
    const configFilenames = this.buildConfigFilenames(environment);
    let currentPath = startPath;

    while (true) {
      for (const filename of configFilenames) {
        const configPath = join(currentPath, this.CONFIG_DIR, filename);
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
   * Loads configuration from a file
   */
  static async loadFromFile(
    configPath: string,
  ): Promise<Record<string, unknown>> {
    try {
      if (!await exists(configPath)) {
        throw new ConfigurationError(
          `Configuration file not found: ${configPath}`,
        );
      }

      const content = await Deno.readTextFile(configPath);
      const parsed = parse(content);

      if (!parsed || typeof parsed !== "object") {
        throw new ConfigurationError(
          "Configuration file must contain a valid YAML object",
        );
      }

      return parsed as Record<string, unknown>;
    } catch (error) {
      if (error instanceof ConfigurationError) {
        throw error;
      }

      throw new ConfigurationError(
        `Failed to load configuration from ${configPath}: ${
          error instanceof Error ? error.message : String(error)
        }`,
      );
    }
  }

  /**
   * Loads configuration with environment support
   */
  static async loadConfig(
    environment?: string,
    configPath?: string,
    startPath?: string,
  ): Promise<{ config: Record<string, unknown>; path: string }> {
    let actualConfigPath: string;

    if (configPath) {
      // Use provided path
      actualConfigPath = configPath;
    } else {
      // Search for config file
      const foundPath = await this.findConfigFile(environment, startPath);
      if (!foundPath) {
        throw new ConfigurationError(
          this.buildConfigNotFoundMessage(environment),
        );
      }
      actualConfigPath = foundPath;
    }

    const config = await this.loadFromFile(actualConfigPath);
    return { config, path: actualConfigPath };
  }

  /**
   * Builds possible configuration filenames for an environment
   */
  private static buildConfigFilenames(environment?: string): string[] {
    const filenames: string[] = [];

    // Environment-specific files first
    if (environment) {
      for (const base of this.DEFAULT_CONFIG_FILES) {
        for (const ext of this.CONFIG_EXTENSIONS) {
          filenames.push(`${base}.${environment}${ext}`);
        }
      }
      // Also try just the environment name
      for (const ext of this.CONFIG_EXTENSIONS) {
        filenames.push(`${environment}${ext}`);
      }
    }

    // Default files
    for (const base of this.DEFAULT_CONFIG_FILES) {
      for (const ext of this.CONFIG_EXTENSIONS) {
        filenames.push(`${base}${ext}`);
      }
    }

    return filenames;
  }

  /**
   * Builds error message for missing configuration
   */
  private static buildConfigNotFoundMessage(environment?: string): string {
    const envStr = environment ? ` for environment '${environment}'` : "";
    const exampleFiles = environment
      ? [
        `.jiji/deploy.${environment}.yml`,
        `.jiji/deploy.${environment}.yaml`,
        `.jiji/${environment}.yml`,
        `.jiji/${environment}.yaml`,
        ".jiji/deploy.yml",
        ".jiji/deploy.yaml",
      ]
      : [".jiji/deploy.yml", ".jiji/deploy.yaml"];

    return `No jiji configuration file found${envStr}. ` +
      `Please create one of the following files:\n${
        exampleFiles.map((f) => `  - ${f}`).join("\n")
      }`;
  }

  /**
   * Validates that a configuration path is readable
   */
  static async validateConfigPath(path: string): Promise<void> {
    try {
      const stat = await Deno.stat(path);
      if (!stat.isFile) {
        throw new ConfigurationError(
          `Configuration path is not a file: ${path}`,
        );
      }
    } catch (error) {
      if (error instanceof Deno.errors.NotFound) {
        throw new ConfigurationError(
          `Configuration file not found: ${path}`,
        );
      }
      if (error instanceof Deno.errors.PermissionDenied) {
        throw new ConfigurationError(
          `Permission denied reading configuration file: ${path}`,
        );
      }
      throw error;
    }
  }

  /**
   * Gets all available configuration files in a directory
   */
  static async getAvailableConfigs(
    searchPath: string = Deno.cwd(),
  ): Promise<string[]> {
    const configs: string[] = [];
    const configDir = join(searchPath, this.CONFIG_DIR);

    try {
      if (!await exists(configDir)) {
        return configs;
      }

      for await (const entry of Deno.readDir(configDir)) {
        if (entry.isFile && this.isConfigFile(entry.name)) {
          configs.push(join(configDir, entry.name));
        }
      }
    } catch {
      // Ignore errors when reading directory
    }

    return configs.sort();
  }

  /**
   * Checks if a filename is a valid configuration file
   */
  private static isConfigFile(filename: string): boolean {
    return this.CONFIG_EXTENSIONS.some((ext) => filename.endsWith(ext));
  }

  /**
   * Extracts environment name from configuration filename
   */
  static extractEnvironment(configPath: string): string | undefined {
    const basename = configPath.split("/").pop() || "";
    const nameWithoutExt = basename.replace(/\.(yml|yaml)$/, "");

    // Check if it's environment-specific (e.g., deploy.production.yml)
    const parts = nameWithoutExt.split(".");
    if (parts.length === 2 && this.DEFAULT_CONFIG_FILES.includes(parts[0])) {
      return parts[1];
    }

    // Check if it's just an environment name (e.g., production.yml)
    if (
      parts.length === 1 && !this.DEFAULT_CONFIG_FILES.includes(parts[0])
    ) {
      return parts[0];
    }

    return undefined;
  }

  /**
   * Merges multiple configuration objects
   */
  static mergeConfigs(
    base: Record<string, unknown>,
    ...overrides: Record<string, unknown>[]
  ): Record<string, unknown> {
    const result = { ...base };

    for (const override of overrides) {
      for (const [key, value] of Object.entries(override)) {
        if (
          value && typeof value === "object" && !Array.isArray(value) &&
          result[key] && typeof result[key] === "object" &&
          !Array.isArray(result[key])
        ) {
          // Deep merge for objects
          result[key] = this.mergeConfigs(
            result[key] as Record<string, unknown>,
            value as Record<string, unknown>,
          );
        } else {
          // Replace for primitives and arrays
          result[key] = value;
        }
      }
    }

    return result;
  }
}
