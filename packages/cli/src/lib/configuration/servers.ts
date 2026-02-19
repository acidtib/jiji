import { BaseConfiguration, ConfigurationError } from "./base.ts";
import type { Validatable } from "./base.ts";
import type { ValidationResult } from "./validation.ts";
import { EnvLoader } from "../../utils/env_loader.ts";

/**
 * Named server configuration from the top-level servers section
 */
export interface NamedServerConfig {
  host: string;
  arch?: string;
  user?: string;
  port?: number;
  key_path?: string;
  key_passphrase?: string;
  keys?: string[];
  key_data?: string[];
}

/**
 * Resolved server configuration with merged SSH settings
 */
export interface ResolvedServerConfig {
  name: string;
  host: string;
  arch: string;
  ssh: {
    user: string;
    port: number;
    key_path?: string;
    key_passphrase?: string;
    keys?: string[];
    key_data?: string[];
  };
}

/**
 * Configuration for the top-level servers section
 */
export class ServersConfiguration extends BaseConfiguration
  implements Validatable {
  private _servers?: Map<string, NamedServerConfig>;
  private _envVars?: Record<string, string>;

  /**
   * Get all named servers
   */
  get servers(): Map<string, NamedServerConfig> {
    if (!this._servers) {
      this._servers = new Map();
      const raw = this.raw as Record<string, unknown>;

      for (const [name, config] of Object.entries(raw)) {
        const serverConfig = this.parseServerConfig(name, config);
        this._servers.set(name, serverConfig);
      }
    }
    return this._servers;
  }

  /**
   * Get a server by name
   */
  getServer(name: string): NamedServerConfig | undefined {
    return this.servers.get(name);
  }

  /**
   * Get all server names
   */
  getAllServerNames(): string[] {
    return Array.from(this.servers.keys()).sort();
  }

  /**
   * Get all unique hostnames
   */
  getAllHosts(): string[] {
    const hosts = new Set<string>();
    for (const server of this.servers.values()) {
      hosts.add(server.host);
    }
    return Array.from(hosts).sort();
  }

  /**
   * Parse and validate a server configuration
   */
  private parseServerConfig(
    name: string,
    config: unknown,
  ): NamedServerConfig {
    const obj = this.validateObject(config, `servers.${name}`);

    // Validate server name is DNS-safe
    this.validateServerName(name);

    // Validate required host
    if (!obj.host || typeof obj.host !== "string") {
      throw new ConfigurationError(
        `'host' is required for server '${name}' in servers section`,
      );
    }

    const serverConfig: NamedServerConfig = {
      host: this.resolveHostValue(obj.host, name),
    };

    // Optional arch
    if (obj.arch !== undefined) {
      if (typeof obj.arch !== "string") {
        throw new ConfigurationError(
          `'arch' for server '${name}' must be a string`,
        );
      }
      const validArchs = ["amd64", "arm64"] as const;
      if (!validArchs.includes(obj.arch as typeof validArchs[number])) {
        throw new ConfigurationError(
          `'arch' for server '${name}' must be 'amd64' or 'arm64', got '${obj.arch}'`,
        );
      }
      serverConfig.arch = obj.arch;
    }

    // Optional SSH overrides
    if (obj.user !== undefined) {
      serverConfig.user = this.validateString(
        obj.user,
        `servers.${name}.user`,
      );
    }

    if (obj.port !== undefined) {
      serverConfig.port = this.validatePort(
        obj.port,
        `servers.${name}.port`,
      );
    }

    if (obj.key_path !== undefined) {
      serverConfig.key_path = this.validateString(
        obj.key_path,
        `servers.${name}.key_path`,
      );
    }

    if (obj.key_passphrase !== undefined) {
      serverConfig.key_passphrase = this.validateString(
        obj.key_passphrase,
        `servers.${name}.key_passphrase`,
      );
    }

    if (obj.keys !== undefined) {
      if (!Array.isArray(obj.keys)) {
        throw new ConfigurationError(
          `'keys' for server '${name}' must be an array`,
        );
      }
      serverConfig.keys = obj.keys.map((key, i) =>
        this.validateString(key, `servers.${name}.keys[${i}]`)
      );
    }

    if (obj.key_data !== undefined) {
      if (!Array.isArray(obj.key_data)) {
        throw new ConfigurationError(
          `'key_data' for server '${name}' must be an array`,
        );
      }
      serverConfig.key_data = obj.key_data.map((key, i) =>
        this.validateString(key, `servers.${name}.key_data[${i}]`)
      );
    }

    return serverConfig;
  }

  /**
   * Resolve a host value, checking if it's an environment variable reference
   */
  private resolveHostValue(host: string, serverName: string): string {
    if (!EnvLoader.isEnvVarReference(host)) {
      return host;
    }

    // Try pre-loaded env vars first, then host environment
    const envVars = this._envVars ?? {};
    const resolved = envVars[host] ?? Deno.env.get(host);

    if (!resolved) {
      throw new ConfigurationError(
        `Environment variable '${host}' not found for server '${serverName}' host. ` +
          `Set it in your .env file or host environment.`,
      );
    }

    return resolved;
  }

  /**
   * Set pre-loaded environment variables for host resolution
   */
  setEnvVars(envVars: Record<string, string>): void {
    this._envVars = envVars;
    // Reset cached servers so they get re-parsed with env vars
    this._servers = undefined;
  }

  /**
   * Validate server name is DNS-safe
   */
  private validateServerName(name: string): void {
    // DNS-safe: alphanumeric and hyphens only
    // Cannot start or end with hyphen
    // Max 63 chars
    const dnsPattern = /^[a-zA-Z0-9]([a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?$/;

    if (!dnsPattern.test(name)) {
      throw new ConfigurationError(
        `Server name '${name}' is not DNS-safe. ` +
          `Must be alphanumeric with hyphens, cannot start/end with hyphen, max 63 chars.`,
      );
    }
  }

  /**
   * Validate the servers configuration
   */
  validate(): ValidationResult {
    const result: ValidationResult = {
      valid: true,
      errors: [],
      warnings: [],
    };

    // Check for duplicate hosts
    const hostMap = new Map<string, string[]>();
    for (const [name, server] of this.servers.entries()) {
      if (!hostMap.has(server.host)) {
        hostMap.set(server.host, []);
      }
      hostMap.get(server.host)!.push(name);
    }

    for (const [host, names] of hostMap.entries()) {
      if (names.length > 1) {
        result.errors.push({
          path: "servers",
          message:
            `Duplicate host '${host}' found in servers: ${names.join(", ")}. ` +
            `Each server must have a unique hostname.`,
          code: "DUPLICATE_HOST",
        });
        result.valid = false;
      }
    }

    return result;
  }
}
