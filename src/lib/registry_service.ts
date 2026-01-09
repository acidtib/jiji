import { log } from "../utils/logger.ts";
import { loadConfig } from "../utils/config.ts";
import { RegistryManager } from "../utils/registry_manager.ts";
import { RegistryAuthenticator } from "./registry_authenticator.ts";
import type { SSHManager } from "../utils/ssh.ts";
import type { ContainerEngine } from "./configuration/builder.ts";
import type { RegistryConfiguration } from "./configuration/registry.ts";
import {
  createRegistryError,
  getErrorMessage,
  RegistryErrorCodes,
} from "../utils/error_handling.ts";
import { DEFAULT_LOCAL_REGISTRY_PORT } from "../constants.ts";
import type {
  AuthenticationResult,
  RegistryCredentials,
  RegistryInfo,
  RegistryOperationResult,
  RegistrySetupOptions,
  RegistryStatus,
} from "../types.ts";

/**
 * Main registry service that encapsulates all registry operations
 * Now stateless and driven by jiji.yml configuration
 */
export class RegistryService {
  private authenticator: RegistryAuthenticator;
  private registryManager?: RegistryManager;
  private engine: ContainerEngine;
  private registryConfig?: RegistryConfiguration;

  constructor(engine?: ContainerEngine) {
    // Initialize engine from provided one or default to docker
    // It will be updated in initialize() when config is loaded
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
      this.registryConfig = config.builder?.registry;

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
   * Remove a registry (Stop local container or logout remote)
   */
  async removeRegistry(url: string): Promise<RegistryOperationResult> {
    log.info(`Removing registry: ${url}`, "registry:service");

    try {
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
      const registryType = this.detectRegistryType(url);
      if (registryType === "local") {
        await this.removeLocalRegistry(url, undefined);
      }

      // We no longer remove from configuration/persistence file
      log.info(
        `Successfully removed/logged out registry: ${url}`,
        "registry:service",
      );

      return {
        success: true,
        registry: url,
        operation: "remove",
        message: "Registry removed/logged out successfully",
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
      // We no longer save configuration state
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
   * Logout from a registry
   */
  async logout(url: string): Promise<AuthenticationResult> {
    log.info(`Logging out from registry: ${url}`, "registry:service");

    try {
      await this.authenticator.logout(url);
      return {
        success: true,
        registry: url,
        message: "Successfully logged out",
        authenticated: false,
        timestamp: new Date().toISOString(),
      };
    } catch (error) {
      throw createRegistryError(
        `Logout failed: ${getErrorMessage(error)}`,
        RegistryErrorCodes.AUTH_FAILED,
        url,
        "logout",
        error instanceof Error ? error : undefined,
      );
    }
  }

  /**
   * Authenticate to a registry on remote servers via SSH
   */
  async authenticateOnRemoteServers(
    url: string,
    credentials: RegistryCredentials | undefined,
    sshManagers: SSHManager[],
  ): Promise<{ success: boolean; errors: string[] }> {
    log.info(
      `Authenticating to registry on ${sshManagers.length} remote server(s)`,
      "registry:service",
    );

    const errors: string[] = [];

    for (const ssh of sshManagers) {
      try {
        const host = ssh.getHost();
        log.debug(`Authenticating to ${url} on ${host}`, "registry:service");

        // Build the login command
        let loginCommand = `${this.engine} login ${url}`;
        if (credentials) {
          loginCommand +=
            ` --username ${credentials.username} --password-stdin`;
        }

        // Execute the login command on the remote server
        if (credentials) {
          // Pass password via stdin
          const result = await ssh.executeCommand(
            `echo "${credentials.password}" | ${loginCommand}`,
          );
          if (result.code !== 0) {
            errors.push(`${host}: ${result.stderr || "Login failed"}`);
          }
        } else {
          const result = await ssh.executeCommand(loginCommand);
          if (result.code !== 0) {
            errors.push(`${host}: ${result.stderr || "Login failed"}`);
          }
        }

        log.debug(`Successfully authenticated on ${host}`, "registry:service");
      } catch (error) {
        const host = ssh.getHost();
        errors.push(`${host}: ${getErrorMessage(error)}`);
      }
    }

    return {
      success: errors.length === 0,
      errors,
    };
  }

  /**
   * Logout from a registry on remote servers via SSH
   */
  async logoutFromRemoteServers(
    url: string,
    sshManagers: SSHManager[],
  ): Promise<{ success: boolean; errors: string[] }> {
    log.info(
      `Logging out from registry on ${sshManagers.length} remote server(s)`,
      "registry:service",
    );

    const errors: string[] = [];

    for (const ssh of sshManagers) {
      try {
        const host = ssh.getHost();
        log.debug(`Logging out from ${url} on ${host}`, "registry:service");

        const logoutCommand = `${this.engine} logout ${url}`;
        const result = await ssh.executeCommand(logoutCommand);

        if (result.code !== 0) {
          errors.push(`${host}: ${result.stderr || "Logout failed"}`);
        }

        log.debug(`Successfully logged out from ${host}`, "registry:service");
      } catch (error) {
        const host = ssh.getHost();
        errors.push(`${host}: ${getErrorMessage(error)}`);
      }
    }

    return {
      success: errors.length === 0,
      errors,
    };
  }

  /**
   * Get registry information
   */
  async getRegistryInfo(url: string): Promise<RegistryInfo | null> {
    try {
      if (!this.registryConfig) {
        return null;
      }

      // Check if the requested URL matches our configured registry
      const configuredUrl = this.registryConfig.getRegistryUrl();
      if (configuredUrl !== url) {
        // If we want to support checking status of registries not in config (e.g. older ones),
        // we could try to detect type from URL, but general policy is we only know about configured one.
        // However, existing semantics might expect us to check status if possible.
        // For now, let's stick to only returning info for the configured registry to be consistent.
        // Or strictly strictly only support the single configured registry.
        return null;
      }

      const status = await this.getRegistryStatus(
        url,
        this.registryConfig.type,
      );

      return {
        url: this.registryConfig.getRegistryUrl(),
        type: this.registryConfig.type,
        port: this.registryConfig.port,
        username: this.registryConfig.username,
        isDefault: true, // The configured registry is always default effectively
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
   * Get the currently configured registry from jiji.yml
   */
  async getConfiguredRegistry(): Promise<RegistryInfo | null> {
    try {
      if (!this.registryConfig) {
        return null;
      }

      const url = this.registryConfig.getRegistryUrl();
      const status = await this.getRegistryStatus(
        url,
        this.registryConfig.type,
      );

      return {
        url: url,
        type: this.registryConfig.type,
        port: this.registryConfig.port,
        username: this.registryConfig.username,
        isDefault: true,
        status,
      };
    } catch (error) {
      throw createRegistryError(
        `Failed to get configured registry: ${getErrorMessage(error)}`,
        RegistryErrorCodes.OPERATION_FAILED,
        undefined,
        "list",
        error instanceof Error ? error : undefined,
      );
    }
  }

  /**
   * Get the default registry
   */
  async getDefaultRegistry(): Promise<RegistryInfo | null> {
    if (!this.registryConfig) {
      return null;
    }
    return await this.getRegistryInfo(this.registryConfig.getRegistryUrl());
  }

  /**
   * Setup local registry
   */
  private async setupLocalRegistry(
    url: string,
    options: RegistrySetupOptions,
  ): Promise<RegistryOperationResult> {
    const port = options.port || this.extractPort(url) ||
      DEFAULT_LOCAL_REGISTRY_PORT;

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
    // config removed as arg since we deduce from URL or manager
    _config?: unknown,
  ): Promise<void> {
    const port = this.extractPort(url) || DEFAULT_LOCAL_REGISTRY_PORT;

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
    type: "local" | "remote",
  ): Promise<RegistryStatus> {
    try {
      if (type === "local") {
        const port = this.extractPort(url) || DEFAULT_LOCAL_REGISTRY_PORT;

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
