import {
  BaseConfiguration,
  ConfigurationError,
  type Validatable,
} from "./base.ts";

/**
 * Environment configuration for managing deployment environments
 */
export class EnvironmentConfiguration extends BaseConfiguration
  implements Validatable {
  private _clear?: Record<string, string>;
  private _secrets?: string[];

  constructor(config: Record<string, unknown> = {}) {
    super(config);
  }

  /**
   * Clear text environment variables
   */
  get clear(): Record<string, string> {
    if (!this._clear) {
      this._clear = this.has("clear")
        ? this.validateObject(this.get("clear"), "clear") as Record<
          string,
          string
        >
        : {};
    }
    return this._clear;
  }

  /**
   * Secret names to load from environment
   */
  get secrets(): string[] {
    if (!this._secrets) {
      this._secrets = this.has("secrets")
        ? this.validateArray<string>(this.get("secrets"), "secrets")
        : [];
    }
    return this._secrets;
  }

  /**
   * Validates the environment configuration
   */
  validate(): void {
    // Validate clear variables
    const vars = this.clear;
    for (const [key, value] of Object.entries(vars)) {
      if (typeof value !== "string") {
        throw new ConfigurationError(
          `Environment variable '${key}' must be a string`,
        );
      }
      if (!this.isValidEnvVarName(key)) {
        throw new ConfigurationError(
          `Invalid environment variable name '${key}'. Must contain only alphanumeric characters and underscores.`,
        );
      }
    }

    // Validate secrets
    for (const secret of this.secrets) {
      if (typeof secret !== "string" || !secret.trim()) {
        throw new ConfigurationError(
          `Secret name '${secret}' must be a non-empty string`,
        );
      }
      if (!this.isValidEnvVarName(secret)) {
        throw new ConfigurationError(
          `Invalid secret name '${secret}'. Must contain only alphanumeric characters and underscores.`,
        );
      }
    }
  }

  /**
   * Validates environment variable name format
   */
  private isValidEnvVarName(name: string): boolean {
    return /^[A-Za-z_][A-Za-z0-9_]*$/.test(name);
  }

  /**
   * Returns the environment configuration as a plain object
   */
  toObject(): Record<string, unknown> {
    const result: Record<string, unknown> = {};

    if (Object.keys(this.clear).length > 0) {
      result.clear = this.clear;
    }

    if (this.secrets.length > 0) {
      result.secrets = this.secrets;
    }

    return result;
  }

  /**
   * Merges with another environment configuration
   * Other configuration takes precedence for clear variables
   * Secrets are combined and deduplicated
   */
  merge(other: EnvironmentConfiguration): EnvironmentConfiguration {
    const merged = new EnvironmentConfiguration({
      clear: { ...this.clear, ...other.clear },
      secrets: [...new Set([...this.secrets, ...other.secrets])],
    });

    return merged;
  }

  /**
   * Gets all environment variables including resolved secrets
   */
  resolveVariables(): Record<string, string> {
    const resolved = { ...this.clear };

    // Resolve secrets from environment
    for (const secret of this.secrets) {
      const value = Deno.env.get(secret);
      if (value !== undefined) {
        resolved[secret] = value;
      }
    }

    return resolved;
  }

  /**
   * Converts environment variables to array format for container execution
   */
  toEnvArray(): string[] {
    const vars = this.resolveVariables();
    return Object.entries(vars).map(([key, value]) => `${key}=${value}`);
  }

  /**
   * Creates an empty environment configuration
   */
  static empty(): EnvironmentConfiguration {
    return new EnvironmentConfiguration({
      clear: {},
      secrets: [],
    });
  }
}
