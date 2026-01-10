import { BaseConfiguration, ConfigurationError } from "./base.ts";
import { DEFAULT_LOCAL_REGISTRY_PORT } from "../../constants.ts";

/**
 * Registry type options
 */
export type RegistryType = "local" | "remote";

/**
 * Configuration for container image registry
 * Supports both local registry (with SSH port forwarding) and remote registries
 */
export class RegistryConfiguration extends BaseConfiguration {
  private static readonly DEFAULT_LOCAL_PORT = DEFAULT_LOCAL_REGISTRY_PORT;
  private static readonly SERVER_PATTERN = /^[a-z0-9.-]+(:\d+)?$/i;

  get type(): RegistryType {
    const value = this.get<string>("type", "local");
    return this.validateEnum(
      value as RegistryType,
      ["local", "remote"] as const,
      "type",
      "registry",
    );
  }

  get port(): number {
    const value = this.get<number>(
      "port",
      RegistryConfiguration.DEFAULT_LOCAL_PORT,
    );
    return this.validatePort(value, "port", "registry");
  }

  get server(): string | undefined {
    const value = this.get<string>("server");
    return value;
  }

  get username(): string | undefined {
    const value = this.get<string>("username");
    return value;
  }

  /**
   * Get the raw password value from config
   * This may be a literal password or an env var name (ALL_CAPS format)
   */
  get password(): string | undefined {
    return this.get<string>("password");
  }

  /**
   * Check if a value looks like an environment variable reference
   * (ALL_CAPS_WITH_UNDERSCORES pattern)
   */
  private isEnvVarReference(value: string): boolean {
    return /^[A-Z][A-Z0-9_]*$/.test(value);
  }

  /**
   * Resolve the password using environment variables
   * Supports:
   * - Bare VAR_NAME: password: REGISTRY_PASSWORD (resolved from .env)
   * - Literal values: password: "my-literal-password"
   *
   * @param envVars Pre-loaded environment variables from .env file
   * @param allowHostEnv Whether to fallback to host environment variables
   * @throws ConfigurationError if password is a var reference that cannot be resolved
   */
  resolvePassword(
    envVars: Record<string, string> = {},
    allowHostEnv: boolean = false,
  ): string | undefined {
    const rawPassword = this.password;
    if (!rawPassword) {
      return undefined;
    }

    // Bare VAR_NAME (ALL_CAPS pattern) - resolve from env
    if (this.isEnvVarReference(rawPassword)) {
      if (envVars[rawPassword] !== undefined) {
        return envVars[rawPassword];
      }
      if (allowHostEnv) {
        const hostValue = Deno.env.get(rawPassword);
        if (hostValue !== undefined) {
          return hostValue;
        }
      }
      throw new ConfigurationError(
        `Environment variable '${rawPassword}' not found for registry password. ` +
          `Create a .env file with this secret, or use --host-env flag.`,
      );
    }

    // Literal password value
    return rawPassword;
  }

  /**
   * Check if this is a local registry configuration
   */
  isLocal(): boolean {
    return this.type === "local";
  }

  /**
   * Get the registry URL (either localhost:port or remote server)
   */
  getRegistryUrl(): string {
    if (this.isLocal()) {
      return `localhost:${this.port}`;
    }
    return this.server!;
  }

  /**
   * Build full image name with registry prefix
   */
  getFullImageName(
    project: string,
    service: string,
    version: string,
  ): string {
    const registry = this.getRegistryUrl();
    const imageName = `${project}-${service}:${version}`;
    const namespace = this.getNamespaceForRegistry(project);

    if (namespace) {
      return `${registry}/${namespace}/${imageName}`;
    }

    return `${registry}/${imageName}`;
  }

  /**
   * Get the appropriate namespace for the current registry
   */
  private getNamespaceForRegistry(_project: string): string | undefined {
    if (!this.server) {
      return undefined;
    }

    const serverHost = this.server.split(":")[0];

    switch (serverHost) {
      case "ghcr.io": {
        // For GHCR: username format (project is already in image name)
        const username = this.username;
        if (username) {
          return username;
        }
        throw new ConfigurationError(
          `GHCR requires username to be configured`,
        );
      }

      case "docker.io":
      case "registry-1.docker.io":
      case "index.docker.io": {
        // Docker Hub: username format
        const dockerUsername = this.username;
        if (dockerUsername) {
          return dockerUsername;
        }
        throw new ConfigurationError(
          `Docker Hub requires username to be configured`,
        );
      }

      default:
        // Other registries don't require namespace
        return undefined;
    }
  }

  /**
   * Validate registry configuration
   */
  validate(): void {
    if (this.type === "remote") {
      // Remote registry requires server, username, and password
      if (!this.server) {
        throw new ConfigurationError(
          "Remote registry requires 'server' to be configured",
        );
      }

      if (!RegistryConfiguration.SERVER_PATTERN.test(this.server)) {
        throw new ConfigurationError(
          `Invalid registry server format: '${this.server}'. Expected format: hostname:port`,
        );
      }

      // Username validation with registry-specific messages
      if (!this.username) {
        const serverHost = this.server.split(":")[0];
        if (serverHost === "ghcr.io") {
          throw new ConfigurationError(
            "GHCR requires username to be configured",
          );
        } else if (
          ["docker.io", "registry-1.docker.io", "index.docker.io"].includes(
            serverHost,
          )
        ) {
          throw new ConfigurationError(
            "Docker Hub requires username to be configured",
          );
        } else {
          throw new ConfigurationError(
            "Remote registry requires 'username' to be configured",
          );
        }
      }

      if (!this.password) {
        throw new ConfigurationError(
          "Remote registry requires 'password' to be configured",
        );
      }
    } else {
      // Local registry - port already validated by validatePort in getter
      // No additional validation needed
    }
  }
}
