import { BaseConfiguration, ConfigurationError } from "./base.ts";

/**
 * Registry type options
 */
export type RegistryType = "local" | "remote";

/**
 * Configuration for container image registry
 * Supports both local registry (with SSH port forwarding) and remote registries
 */
export class RegistryConfiguration extends BaseConfiguration {
  private static readonly DEFAULT_LOCAL_PORT = 5000;
  private static readonly SERVER_PATTERN = /^[a-z0-9.-]+:\d+$/i;

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

  get password(): string | undefined {
    const rawPassword = this.get<string>("password");
    if (!rawPassword) {
      return undefined;
    }

    // Support environment variable substitution
    if (rawPassword.startsWith("${") && rawPassword.endsWith("}")) {
      const envVar = rawPassword.slice(2, -1);
      const envValue = Deno.env.get(envVar);
      if (!envValue) {
        throw new ConfigurationError(
          `Environment variable '${envVar}' not found for registry password`,
        );
      }
      return envValue;
    }

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
    return `${registry}/${project}-${service}:${version}`;
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

      if (!this.username) {
        throw new ConfigurationError(
          "Remote registry requires 'username' to be configured",
        );
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
