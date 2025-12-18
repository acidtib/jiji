import { BaseConfiguration, ConfigurationError } from "./base.ts";
import { RegistryConfiguration } from "./registry.ts";

/**
 * Container engine options
 */
export type ContainerEngine = "docker" | "podman";

/**
 * Builder configuration for building container images
 * Supports local and remote builds with flexible registry options
 */
export class BuilderConfiguration extends BaseConfiguration {
  private static readonly SSH_URI_PATTERN =
    /^ssh:\/\/(?:([^@]+)@)?([^:]+)(?::(\d+))?$/;
  private _registryConfig?: RegistryConfiguration;

  get engine(): ContainerEngine {
    const value = this.getRequired<string>("engine");
    return this.validateEnum(
      value as ContainerEngine,
      ["docker", "podman"] as const,
      "engine",
      "builder",
    );
  }

  get local(): boolean {
    return this.get<boolean>("local", true);
  }

  get remote(): string | undefined {
    return this.get<string>("remote");
  }

  get cache(): boolean {
    return this.get<boolean>("cache", true);
  }

  /**
   * Get registry configuration
   */
  get registry(): RegistryConfiguration {
    if (!this._registryConfig) {
      const registryRaw = this.get<Record<string, unknown>>("registry", {
        type: "local",
      });
      this._registryConfig = new RegistryConfiguration(registryRaw);
    }
    return this._registryConfig;
  }

  /**
   * Check if this is a local build
   */
  isLocalBuild(): boolean {
    return this.local && !this.remote;
  }

  /**
   * Parse remote SSH URI and return connection details
   */
  getRemoteHost(): { user: string; host: string; port: number } | null {
    if (!this.remote) {
      return null;
    }

    const match = this.remote.match(BuilderConfiguration.SSH_URI_PATTERN);
    if (!match) {
      return null;
    }

    const [, user, host, portStr] = match;
    return {
      user: user || "root",
      host: host,
      port: portStr ? parseInt(portStr) : 22,
    };
  }

  /**
   * Validate builder configuration
   */
  validate(): void {
    // Validate remote SSH URI format if specified
    if (this.remote) {
      if (!BuilderConfiguration.SSH_URI_PATTERN.test(this.remote)) {
        throw new ConfigurationError(
          `Invalid remote builder URI: '${this.remote}'. ` +
            `Expected format: ssh://user@host:port`,
        );
      }

      // If remote is set, local should be false
      if (this.local) {
        throw new ConfigurationError(
          "Builder cannot be both 'local: true' and have a 'remote' configuration. " +
            "Set 'local: false' or remove the 'remote' field.",
        );
      }
    }

    // Validate registry configuration
    this.registry.validate();
  }
}
