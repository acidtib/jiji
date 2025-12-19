import { log } from "../utils/logger.ts";
import { loadConfig } from "../utils/config.ts";
import { RegistryManager } from "../utils/registry_manager.ts";
import {
  type RegistryConfig,
  RegistryConfigManager,
} from "../utils/registry_config.ts";
import {
  type AuthenticationResult,
  RegistryAuthenticator,
  type RegistryCredentials,
} from "./registry_authenticator.ts";

import type { ContainerEngine } from "./configuration/builder.ts";
import {
  createRegistryError,
  getErrorMessage,
  RegistryErrorCodes,
} from "../utils/error_handling.ts";

/**
 * Registry information interface
 */
export interface RegistryInfo {
  url: string;
  type: "local" | "remote";
  port?: number;
  username?: string;
  isDefault: boolean;
  lastLogin?: string;
  status: RegistryStatus;
}

/**
 * Registry status information
 */
export interface RegistryStatus {
  available: boolean;
  authenticated: boolean;
  running?: boolean; // For local registries
  containerId?: string; // For local registries
  message?: string;
}

/**
 * Registry setup options
 */
export interface RegistrySetupOptions {
  type?: "local" | "remote";
  port?: number;
  credentials?: RegistryCredentials;
  isDefault?: boolean;
  skipAuthentication?: boolean;
}

/**
 * Registry operation result
 */
export interface RegistryOperationResult {
  success: boolean;
  registry: string;
  operation: string;
  message?: string;
  data?: Record<string, unknown>;
}

/**
 * Main registry service that encapsulates all registry operations
 */
export class RegistryService {
  private configManager: RegistryConfigManager;
  private authenticator: RegistryAuthenticator;
  private registryManager?: RegistryManager;
  private engine: ContainerEngine;

  constructor(engine?: ContainerEngine) {
    this.configManager = new RegistryConfigManager();
    // Initialize engine from config or use provided one
    this.engine = engine || "docker";
    this.authenticator = new RegistryAuthenticator(this.engine);
  }

  /**
   * Initialize the service with configuration
   */
  async initialize(): Promise<void> {
    try {
      const { config } = await loadConfig();
      this.engine = config.builder?.engine || "docker";
      this.authenticator = new RegistryAuthenticator(this.engine);

      log.debug(
        `Initialized RegistryService with engine: ${this.engine}`,
        "registry:service",
      );
    } catch (error) {
      log.warn(
        `Failed to load config, using defaults: ${getErrorMessage(error)}`,
        "registry:service",
      );
    }
  }

  /**
   * Setup a registry (local or remote)
   */
  async setupRegistry(
    url: string,
    options: RegistrySetupOptions = {},
  ): Promise<RegistryOperationResult> {
    log.info(`Setting up registry: ${url}`, "registry:service");

    try {
      const registryType = options.type || this.detectRegistryType(url);

      if (registryType === "local") {
        return await this.setupLocalRegistry(url, options);
      } else {
        return await this.setupRemoteRegistry(url, options);
      }
    } catch (error) {
      throw createRegistryError(
        `Failed to setup registry: ${getErrorMessage(error)}`,
        RegistryErrorCodes.OPERATION_FAILED,
        url,
        "setup",
        error instanceof Error ? error : undefined,
      );
    }
  }

  /**
   * Remove a registry
   */
  async removeRegistry(url: string): Promise<RegistryOperationResult> {
    log.info(`Removing registry: ${url}`, "registry:service");

    try {
      const registryConfig = await this.configManager.getRegistry(url);

      if (!registryConfig) {
        log.warn(
          `Registry not found in configuration: ${url}`,
          "registry:service",
        );
        return {
          success: true,
          registry: url,
          operation: "remove",
          message: "Registry not configured",
        };
      }

      // Logout from registry
      try {
        await this.authenticator.logout(url);
      } catch (error) {
        log.warn(
          `Failed to logout from registry: ${getErrorMessage(error)}`,
          "registry:service",
        );
      }

      // Remove local registry container if it's a local registry
      if (registryConfig.type === "local") {
        await this.removeLocalRegistry(url, registryConfig);
      }

      // Remove from configuration
      await this.configManager.removeRegistry(url);

      log.info(`Successfully removed registry: ${url}`, "registry:service");

      return {
        success: true,
        registry: url,
        operation: "remove",
        message: "Registry removed successfully",
      };
    } catch (error) {
      throw createRegistryError(
        `Failed to remove registry: ${getErrorMessage(error)}`,
        RegistryErrorCodes.OPERATION_FAILED,
        url,
        "remove",
        error instanceof Error ? error : undefined,
      );
    }
  }

  /**
   * Authenticate to a registry
   */
  async authenticate(
    url: string,
    credentials?: RegistryCredentials,
  ): Promise<AuthenticationResult> {
    log.info(`Authenticating to registry: ${url}`, "registry:service");

    try {
      const result = await this.authenticator.login(url, credentials);

      // Update configuration with successful authentication
      if (result.success) {
        const registryType = this.detectRegistryType(url);
        const registryConfig: RegistryConfig = {
          url,
          type: registryType,
          username: credentials?.username,
          port: registryType === "local" ? this.extractPort(url) : undefined,
          isDefault: !(await this.configManager.getDefaultRegistry()),
          lastLogin: new Date().toISOString(),
        };

        await this.configManager.addRegistry(registryConfig);
      }

      return result;
    } catch (error) {
      throw createRegistryError(
        `Authentication failed: ${getErrorMessage(error)}`,
        RegistryErrorCodes.AUTH_FAILED,
        url,
        "authenticate",
        error instanceof Error ? error : undefined,
      );
    }
  }

  /**
   * Get registry information
   */
  async getRegistryInfo(url: string): Promise<RegistryInfo | null> {
    try {
      const config = await this.configManager.getRegistry(url);

      if (!config) {
        return null;
      }

      const status = await this.getRegistryStatus(url, config);

      return {
        url: config.url,
        type: config.type,
        port: config.port,
        username: config.username,
        isDefault: config.isDefault || false,
        lastLogin: config.lastLogin,
        status,
      };
    } catch (error) {
      log.error(
        `Failed to get registry info for ${url}: ${getErrorMessage(error)}`,
        "registry:service",
      );
      return null;
    }
  }

  /**
   * List all configured registries
   */
  async listRegistries(): Promise<RegistryInfo[]> {
    try {
      const configs = await this.configManager.loadConfigs();
      const registries: RegistryInfo[] = [];

      for (const config of configs) {
        const status = await this.getRegistryStatus(config.url, config);

        registries.push({
          url: config.url,
          type: config.type,
          port: config.port,
          username: config.username,
          isDefault: config.isDefault || false,
          lastLogin: config.lastLogin,
          status,
        });
      }

      return registries;
    } catch (error) {
      throw createRegistryError(
        `Failed to list registries: ${getErrorMessage(error)}`,
        RegistryErrorCodes.OPERATION_FAILED,
        undefined,
        "list",
        error instanceof Error ? error : undefined,
      );
    }
  }

  /**
   * Set a registry as the default
   */
  async setDefaultRegistry(url: string): Promise<RegistryOperationResult> {
    try {
      await this.configManager.setDefaultRegistry(url);

      log.info(`Set default registry: ${url}`, "registry:service");

      return {
        success: true,
        registry: url,
        operation: "set-default",
        message: "Default registry set successfully",
      };
    } catch (error) {
      throw createRegistryError(
        `Failed to set default registry: ${getErrorMessage(error)}`,
        RegistryErrorCodes.OPERATION_FAILED,
        url,
        "set-default",
        error instanceof Error ? error : undefined,
      );
    }
  }

  /**
   * Get the default registry
   */
  async getDefaultRegistry(): Promise<RegistryInfo | null> {
    try {
      const config = await this.configManager.getDefaultRegistry();

      if (!config) {
        return null;
      }

      return await this.getRegistryInfo(config.url);
    } catch (error) {
      log.error(
        `Failed to get default registry: ${getErrorMessage(error)}`,
        "registry:service",
      );
      return null;
    }
  }

  /**
   * Setup local registry
   */
  private async setupLocalRegistry(
    url: string,
    options: RegistrySetupOptions,
  ): Promise<RegistryOperationResult> {
    const port = options.port || this.extractPort(url) || 6767;

    try {
      // Initialize registry manager if needed
      if (
        !this.registryManager ||
        this.registryManager.getRegistryUrl() !== `localhost:${port}`
      ) {
        this.registryManager = new RegistryManager(this.engine, port);
      }

      // Start the local registry
      await this.registryManager.start();

      // Save configuration
      const registryConfig: RegistryConfig = {
        url,
        type: "local",
        port,
        isDefault: options.isDefault ??
          !(await this.configManager.getDefaultRegistry()),
        lastLogin: new Date().toISOString(),
      };

      await this.configManager.addRegistry(registryConfig);

      log.info(
        `Local registry setup completed on port ${port}`,
        "registry:service",
      );

      return {
        success: true,
        registry: url,
        operation: "setup-local",
        message: `Local registry started on port ${port}`,
        data: { port },
      };
    } catch (error) {
      throw createRegistryError(
        `Failed to setup local registry: ${getErrorMessage(error)}`,
        RegistryErrorCodes.LOCAL_REGISTRY_START_FAILED,
        url,
        "setup-local",
        error instanceof Error ? error : undefined,
      );
    }
  }

  /**
   * Setup remote registry
   */
  private async setupRemoteRegistry(
    url: string,
    options: RegistrySetupOptions,
  ): Promise<RegistryOperationResult> {
    try {
      // Authenticate if credentials provided and not skipped
      if (options.credentials && !options.skipAuthentication) {
        await this.authenticator.login(url, options.credentials);
      }

      // Save configuration
      const registryConfig: RegistryConfig = {
        url,
        type: "remote",
        username: options.credentials?.username,
        isDefault: options.isDefault ??
          !(await this.configManager.getDefaultRegistry()),
        lastLogin: new Date().toISOString(),
      };

      await this.configManager.addRegistry(registryConfig);

      log.info(`Remote registry setup completed: ${url}`, "registry:service");

      return {
        success: true,
        registry: url,
        operation: "setup-remote",
        message: "Remote registry setup completed",
      };
    } catch (error) {
      throw createRegistryError(
        `Failed to setup remote registry: ${getErrorMessage(error)}`,
        RegistryErrorCodes.OPERATION_FAILED,
        url,
        "setup-remote",
        error instanceof Error ? error : undefined,
      );
    }
  }

  /**
   * Remove local registry
   */
  private async removeLocalRegistry(
    url: string,
    config: RegistryConfig,
  ): Promise<void> {
    const port = config.port || 6767;

    if (
      !this.registryManager ||
      this.registryManager.getRegistryUrl() !== `localhost:${port}`
    ) {
      this.registryManager = new RegistryManager(this.engine, port);
    }

    await this.registryManager.remove();
    log.info(`Local registry container removed for ${url}`, "registry:service");
  }

  /**
   * Get registry status
   */
  private async getRegistryStatus(
    url: string,
    config: RegistryConfig,
  ): Promise<RegistryStatus> {
    try {
      if (config.type === "local") {
        const port = config.port || 6767;

        if (
          !this.registryManager ||
          this.registryManager.getRegistryUrl() !== `localhost:${port}`
        ) {
          this.registryManager = new RegistryManager(this.engine, port);
        }

        const status = await this.registryManager.getStatus();

        return {
          available: true,
          authenticated: true, // Local registries don't require auth
          running: status.running,
          containerId: status.containerId,
          message: status.running ? "Running" : "Stopped",
        };
      } else {
        // For remote registries, check authentication
        const authenticated = await this.authenticator.isAuthenticated(url);

        return {
          available: true, // Assume available unless proven otherwise
          authenticated,
          message: authenticated ? "Authenticated" : "Not authenticated",
        };
      }
    } catch (error) {
      return {
        available: false,
        authenticated: false,
        message: `Error: ${getErrorMessage(error)}`,
      };
    }
  }

  /**
   * Detect registry type based on URL
   */
  private detectRegistryType(url: string): "local" | "remote" {
    return url.includes("localhost") ||
        url.includes("127.0.0.1") ||
        url.includes("0.0.0.0")
      ? "local"
      : "remote";
  }

  /**
   * Extract port from registry URL
   */
  private extractPort(url: string): number | undefined {
    const portMatch = url.match(/:(\d+)$/);
    return portMatch ? parseInt(portMatch[1]) : undefined;
  }

  /**
   * Get the container engine being used
   */
  getEngine(): ContainerEngine {
    return this.engine;
  }
}
