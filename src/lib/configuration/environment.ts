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
  private _name: string;
  private _variables?: Record<string, string>;
  private _secrets?: string[];
  private _files?: Record<string, string>;

  constructor(name: string, config: Record<string, unknown> = {}) {
    super(config);
    this._name = name;
  }

  /**
   * Environment name
   */
  get name(): string {
    return this._name;
  }

  /**
   * Environment variables
   */
  get variables(): Record<string, string> {
    if (!this._variables) {
      this._variables = this.has("variables")
        ? this.validateObject(
          this.get("variables"),
          "variables",
          this.name,
        ) as Record<string, string>
        : {};
    }
    return this._variables;
  }

  /**
   * Secret names to load from environment or external sources
   */
  get secrets(): string[] {
    if (!this._secrets) {
      this._secrets = this.has("secrets")
        ? this.validateArray<string>(this.get("secrets"), "secrets", this.name)
        : [];
    }
    return this._secrets;
  }

  /**
   * Environment files to load
   */
  get files(): Record<string, string> {
    if (!this._files) {
      this._files = this.has("files")
        ? this.validateObject(this.get("files"), "files", this.name) as Record<
          string,
          string
        >
        : {};
    }
    return this._files;
  }

  /**
   * Validates the environment configuration
   */
  validate(): void {
    // Validate variables
    const vars = this.variables;
    for (const [key, value] of Object.entries(vars)) {
      if (typeof value !== "string") {
        throw new ConfigurationError(
          `Environment variable '${key}' in environment '${this.name}' must be a string`,
        );
      }
      if (!this.isValidEnvVarName(key)) {
        throw new ConfigurationError(
          `Invalid environment variable name '${key}' in environment '${this.name}'. Must contain only alphanumeric characters and underscores.`,
        );
      }
    }

    // Validate secrets
    for (const secret of this.secrets) {
      if (typeof secret !== "string" || !secret.trim()) {
        throw new ConfigurationError(
          `Secret name '${secret}' in environment '${this.name}' must be a non-empty string`,
        );
      }
      if (!this.isValidEnvVarName(secret)) {
        throw new ConfigurationError(
          `Invalid secret name '${secret}' in environment '${this.name}'. Must contain only alphanumeric characters and underscores.`,
        );
      }
    }

    // Validate files
    const files = this.files;
    for (const [key, path] of Object.entries(files)) {
      if (typeof path !== "string" || !path.trim()) {
        throw new ConfigurationError(
          `File path for '${key}' in environment '${this.name}' must be a non-empty string`,
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

    if (Object.keys(this.variables).length > 0) {
      result.variables = this.variables;
    }

    if (this.secrets.length > 0) {
      result.secrets = this.secrets;
    }

    if (Object.keys(this.files).length > 0) {
      result.files = this.files;
    }

    return result;
  }

  /**
   * Merges with another environment configuration
   */
  merge(other: EnvironmentConfiguration): EnvironmentConfiguration {
    const merged = new EnvironmentConfiguration(this.name, {
      variables: { ...this.variables, ...other.variables },
      secrets: [...new Set([...this.secrets, ...other.secrets])],
      files: { ...this.files, ...other.files },
    });

    return merged;
  }

  /**
   * Gets all environment variables including resolved secrets
   */
  async resolveVariables(): Promise<Record<string, string>> {
    const resolved = { ...this.variables };

    // Resolve secrets from environment
    for (const secret of this.secrets) {
      const value = Deno.env.get(secret);
      if (value !== undefined) {
        resolved[secret] = value;
      }
    }

    // Load variables from files
    for (const [key, filePath] of Object.entries(this.files)) {
      try {
        const content = await Deno.readTextFile(filePath);
        resolved[key] = content.trim();
      } catch (error) {
        throw new ConfigurationError(
          `Failed to read environment file '${filePath}' for variable '${key}': ${
            error instanceof Error ? error.message : String(error)
          }`,
        );
      }
    }

    return resolved;
  }

  /**
   * Converts environment variables to array format for container execution
   */
  toEnvArray(): string[] {
    const vars = this.variables;
    return Object.entries(vars).map(([key, value]) => `${key}=${value}`);
  }

  /**
   * Creates an environment configuration with defaults
   */
  static withDefaults(
    name: string,
    overrides: Record<string, unknown> = {},
  ): EnvironmentConfiguration {
    return new EnvironmentConfiguration(name, {
      variables: {},
      secrets: [],
      files: {},
      clear: false,
      ...overrides,
    });
  }
}
