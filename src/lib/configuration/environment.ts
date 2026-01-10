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
   * Automatically converts numbers and booleans to strings
   */
  get clear(): Record<string, string> {
    if (!this._clear) {
      const rawClear = this.has("clear")
        ? this.validateObject(this.get("clear"), "clear")
        : {};

      // Convert all values to strings
      this._clear = {};
      for (const [key, value] of Object.entries(rawClear)) {
        if (
          typeof value === "string" || typeof value === "number" ||
          typeof value === "boolean"
        ) {
          this._clear[key] = String(value);
        } else {
          this._clear[key] = value as string; // Will be caught by validation
        }
      }
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
    // Validate clear variables - accessing this.clear converts values to strings
    const rawClear = this.has("clear") ? this.get("clear") : {};
    if (typeof rawClear === "object" && rawClear !== null) {
      for (
        const [key, value] of Object.entries(
          rawClear as Record<string, unknown>,
        )
      ) {
        // Accept string, number, and boolean values
        if (
          typeof value !== "string" && typeof value !== "number" &&
          typeof value !== "boolean"
        ) {
          throw new ConfigurationError(
            `Environment variable '${key}' must be a string, number, or boolean`,
          );
        }
        if (!this.isValidEnvVarName(key)) {
          throw new ConfigurationError(
            `Invalid environment variable name '${key}'. Must contain only alphanumeric characters and underscores.`,
          );
        }
      }
    }

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
   * Check if a value looks like an environment variable reference
   * (ALL_CAPS_WITH_UNDERSCORES pattern)
   */
  isEnvVarReference(value: string): boolean {
    return /^[A-Z][A-Z0-9_]*$/.test(value);
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
   * @param envVars Pre-loaded environment variables from .env file
   * @param allowHostEnv Whether to fallback to host environment variables
   * @throws ConfigurationError if any secrets cannot be resolved
   */
  resolveVariables(
    envVars: Record<string, string> = {},
    allowHostEnv: boolean = false,
  ): Record<string, string> {
    const resolved: Record<string, string> = {};
    const missingSecrets: string[] = [];

    for (const [key, value] of Object.entries(this.clear)) {
      if (this.isEnvVarReference(value)) {
        if (envVars[value] !== undefined) {
          resolved[key] = envVars[value];
        } else if (allowHostEnv) {
          const hostValue = Deno.env.get(value);
          if (hostValue !== undefined) {
            resolved[key] = hostValue;
          } else {
            missingSecrets.push(value);
          }
        } else {
          missingSecrets.push(value);
        }
      } else {
        // Use literal value
        resolved[key] = value;
      }
    }

    // Process secrets - all must be resolved or throw error
    for (const secret of this.secrets) {
      if (envVars[secret] !== undefined) {
        resolved[secret] = envVars[secret];
      } else if (allowHostEnv) {
        const hostValue = Deno.env.get(secret);
        if (hostValue !== undefined) {
          resolved[secret] = hostValue;
        } else {
          missingSecrets.push(secret);
        }
      } else {
        missingSecrets.push(secret);
      }
    }

    if (missingSecrets.length > 0) {
      const uniqueMissing = [...new Set(missingSecrets)];
      throw new ConfigurationError(
        `Missing required secrets: ${uniqueMissing.join(", ")}. ` +
          `Create a .env file with these secrets, or use --host-env flag to read from host environment.`,
      );
    }

    return resolved;
  }

  /**
   * Converts environment variables to array format for container execution
   * @param envVars Pre-loaded environment variables from .env file
   * @param allowHostEnv Whether to fallback to host environment variables
   */
  toEnvArray(
    envVars: Record<string, string> = {},
    allowHostEnv: boolean = false,
  ): string[] {
    const vars = this.resolveVariables(envVars, allowHostEnv);
    return Object.entries(vars).map(([key, value]) => `${key}=${value}`);
  }

  /**
   * Check which secrets are defined but not resolvable
   * Used for debugging/reporting without throwing errors
   * @param envVars Pre-loaded environment variables from .env file
   * @param allowHostEnv Whether to check host environment as fallback
   */
  getMissingSecrets(
    envVars: Record<string, string> = {},
    allowHostEnv: boolean = false,
  ): string[] {
    const missing: string[] = [];

    for (const [_key, value] of Object.entries(this.clear)) {
      if (this.isEnvVarReference(value)) {
        if (envVars[value] === undefined) {
          if (!allowHostEnv || Deno.env.get(value) === undefined) {
            missing.push(value);
          }
        }
      }
    }

    for (const secret of this.secrets) {
      if (envVars[secret] === undefined) {
        if (!allowHostEnv || Deno.env.get(secret) === undefined) {
          missing.push(secret);
        }
      }
    }

    return [...new Set(missing)];
  }

  /**
   * Check if a secret is resolvable
   * @param name The secret name to check
   * @param envVars Pre-loaded environment variables from .env file
   * @param allowHostEnv Whether to check host environment as fallback
   */
  isSecretResolvable(
    name: string,
    envVars: Record<string, string> = {},
    allowHostEnv: boolean = false,
  ): boolean {
    if (envVars[name] !== undefined) {
      return true;
    }
    if (allowHostEnv && Deno.env.get(name) !== undefined) {
      return true;
    }
    return false;
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
