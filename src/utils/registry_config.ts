import { log } from "./logger.ts";
import { exists } from "@std/fs";
import { dirname, join } from "@std/path";

/**
 * Helper function to safely extract error message from unknown error type
 */
function getErrorMessage(error: unknown): string {
  if (error instanceof Error) {
    return error.message;
  }
  return String(error);
}

/**
 * Registry configuration entry
 */
export interface RegistryConfig {
  url: string;
  type: "local" | "remote";
  port?: number;
  username?: string;
  isDefault?: boolean;
  lastLogin?: string;
}

/**
 * Registry configuration manager
 * Handles storing and retrieving registry configurations
 */
export class RegistryConfigManager {
  private static readonly CONFIG_DIR = ".jiji";
  private static readonly CONFIG_FILE = "registry.json";

  private configPath: string;

  constructor(configDir?: string) {
    this.configPath = join(
      configDir || RegistryConfigManager.CONFIG_DIR,
      RegistryConfigManager.CONFIG_FILE,
    );
  }

  /**
   * Load registry configurations from file
   */
  async loadConfigs(): Promise<RegistryConfig[]> {
    try {
      if (!(await exists(this.configPath))) {
        return [];
      }

      const content = await Deno.readTextFile(this.configPath);
      const configs = JSON.parse(content) as RegistryConfig[];

      log.debug(
        `Loaded ${configs.length} registry configurations`,
        "registry:config",
      );

      return configs;
    } catch (error) {
      log.error(
        `Failed to load registry configurations: ${getErrorMessage(error)}`,
        "registry:config",
      );
      return [];
    }
  }

  /**
   * Save registry configurations to file
   */
  async saveConfigs(configs: RegistryConfig[]): Promise<void> {
    try {
      // Ensure config directory exists
      await this.ensureConfigDir();

      const content = JSON.stringify(configs, null, 2);
      await Deno.writeTextFile(this.configPath, content);

      log.debug(
        `Saved ${configs.length} registry configurations`,
        "registry:config",
      );
    } catch (error) {
      throw new Error(
        `Failed to save registry configurations: ${getErrorMessage(error)}`,
      );
    }
  }

  /**
   * Add or update a registry configuration
   */
  async addRegistry(config: RegistryConfig): Promise<void> {
    const configs = await this.loadConfigs();

    // Remove existing config for the same URL
    const filteredConfigs = configs.filter((c) => c.url !== config.url);

    // Add timestamp for last login
    config.lastLogin = new Date().toISOString();

    // If this is marked as default, remove default from others
    if (config.isDefault) {
      filteredConfigs.forEach((c) => c.isDefault = false);
    }

    // Add the new/updated config
    filteredConfigs.push(config);

    await this.saveConfigs(filteredConfigs);

    log.debug(
      `Added/updated registry configuration: ${config.url}`,
      "registry:config",
    );
  }

  /**
   * Remove a registry configuration
   */
  async removeRegistry(url: string): Promise<void> {
    const configs = await this.loadConfigs();
    const filteredConfigs = configs.filter((c) => c.url !== url);

    if (filteredConfigs.length === configs.length) {
      log.warn(`Registry configuration not found: ${url}`, "registry:config");
      return;
    }

    await this.saveConfigs(filteredConfigs);

    log.debug(`Removed registry configuration: ${url}`, "registry:config");
  }

  /**
   * Get a specific registry configuration
   */
  async getRegistry(url: string): Promise<RegistryConfig | undefined> {
    const configs = await this.loadConfigs();
    return configs.find((c) => c.url === url);
  }

  /**
   * Get the default registry configuration
   */
  async getDefaultRegistry(): Promise<RegistryConfig | undefined> {
    const configs = await this.loadConfigs();
    return configs.find((c) => c.isDefault) || configs[0];
  }

  /**
   * Set a registry as the default
   */
  async setDefaultRegistry(url: string): Promise<void> {
    const configs = await this.loadConfigs();

    // Find the target registry
    const targetRegistry = configs.find((c) => c.url === url);
    if (!targetRegistry) {
      throw new Error(`Registry not found: ${url}`);
    }

    // Remove default from all others
    configs.forEach((c) => c.isDefault = false);

    // Set the target as default
    targetRegistry.isDefault = true;

    await this.saveConfigs(configs);

    log.debug(`Set default registry: ${url}`, "registry:config");
  }

  /**
   * Get all registry URLs
   */
  async getAllRegistryUrls(): Promise<string[]> {
    const configs = await this.loadConfigs();
    return configs.map((c) => c.url);
  }

  /**
   * Get all local registries
   */
  async getLocalRegistries(): Promise<RegistryConfig[]> {
    const configs = await this.loadConfigs();
    return configs.filter((c) => c.type === "local");
  }

  /**
   * Get all remote registries
   */
  async getRemoteRegistries(): Promise<RegistryConfig[]> {
    const configs = await this.loadConfigs();
    return configs.filter((c) => c.type === "remote");
  }

  /**
   * Clear all registry configurations
   */
  async clearAll(): Promise<void> {
    await this.saveConfigs([]);
    log.debug("Cleared all registry configurations", "registry:config");
  }

  /**
   * Check if a registry is configured
   */
  async isRegistryConfigured(url: string): Promise<boolean> {
    const config = await this.getRegistry(url);
    return config !== undefined;
  }

  /**
   * Determine if a URL is a local registry
   */
  static isLocalRegistry(url: string): boolean {
    return url.includes("localhost") ||
      url.includes("127.0.0.1") ||
      url.includes("0.0.0.0");
  }

  /**
   * Parse registry URL and extract components
   */
  static parseRegistryUrl(url: string): {
    hostname: string;
    port?: number;
    protocol?: string;
  } {
    try {
      const parsedUrl = new URL(url);
      return {
        hostname: parsedUrl.hostname,
        port: parsedUrl.port ? parseInt(parsedUrl.port) : undefined,
        protocol: parsedUrl.protocol,
      };
    } catch {
      // Handle URLs without protocol
      const portMatch = url.match(/:(\d+)$/);
      const hostname = portMatch ? url.replace(/:(\d+)$/, "") : url;
      const port = portMatch ? parseInt(portMatch[1]) : undefined;

      return {
        hostname,
        port,
      };
    }
  }

  /**
   * Ensure the config directory exists
   */
  private async ensureConfigDir(): Promise<void> {
    const configDir = dirname(this.configPath);

    try {
      await Deno.mkdir(configDir, { recursive: true });
    } catch (error) {
      if (!(error instanceof Deno.errors.AlreadyExists)) {
        throw error;
      }
    }
  }

  /**
   * Validate registry configuration
   */
  static validateConfig(config: Partial<RegistryConfig>): string[] {
    const errors: string[] = [];

    if (!config.url) {
      errors.push("Registry URL is required");
    }

    if (!config.type) {
      errors.push("Registry type is required");
    }

    if (config.type && !["local", "remote"].includes(config.type)) {
      errors.push("Registry type must be 'local' or 'remote'");
    }

    if (config.port && (config.port < 1 || config.port > 65535)) {
      errors.push("Registry port must be between 1 and 65535");
    }

    return errors;
  }
}

/**
 * Get the default registry configuration manager instance
 */
export function getRegistryConfigManager(): RegistryConfigManager {
  return new RegistryConfigManager();
}

/**
 * Helper function to create a registry configuration
 */
export function createRegistryConfig(
  url: string,
  options: {
    username?: string;
    port?: number;
    isDefault?: boolean;
    type?: "local" | "remote";
  } = {},
): RegistryConfig {
  const type = options.type ||
    (RegistryConfigManager.isLocalRegistry(url) ? "local" : "remote");

  return {
    url,
    type,
    username: options.username,
    port: options.port,
    isDefault: options.isDefault || false,
  };
}
