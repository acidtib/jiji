/**
 * Base configuration class providing common functionality for all configuration components
 */
export abstract class BaseConfiguration {
  protected raw: Record<string, unknown>;

  constructor(config: Record<string, unknown> = {}) {
    this.raw = { ...config };
  }

  /**
   * Gets a value from the raw configuration with optional default
   */
  protected get<T>(key: string, defaultValue?: T): T {
    const value = this.raw[key];
    return value !== undefined ? value as T : defaultValue as T;
  }

  /**
   * Checks if a key exists in the configuration
   */
  protected has(key: string): boolean {
    return key in this.raw && this.raw[key] !== undefined;
  }

  /**
   * Gets a required value, throwing an error if not present
   */
  protected getRequired<T>(key: string, context?: string): T {
    if (!this.has(key)) {
      const contextStr = context ? ` in ${context}` : "";
      throw new ConfigurationError(
        `Missing required configuration: '${key}'${contextStr}`,
      );
    }
    return this.get<T>(key);
  }

  /**
   * Validates that a value is one of the allowed options
   */
  protected validateEnum<T extends string>(
    value: T,
    allowedValues: readonly T[],
    key: string,
    context?: string,
  ): T {
    if (!allowedValues.includes(value)) {
      const contextStr = context ? ` in ${context}` : "";
      throw new ConfigurationError(
        `Invalid value for '${key}'${contextStr}: '${value}'. Must be one of: ${
          allowedValues.join(", ")
        }`,
      );
    }
    return value;
  }

  /**
   * Validates that a value is a string
   */
  protected validateString(
    value: unknown,
    key: string,
    context?: string,
  ): string {
    if (typeof value !== "string") {
      const contextStr = context ? ` in ${context}` : "";
      throw new ConfigurationError(`'${key}'${contextStr} must be a string`);
    }
    return value;
  }

  /**
   * Validates that a value is a number
   */
  protected validateNumber(
    value: unknown,
    key: string,
    context?: string,
  ): number {
    if (typeof value !== "number" || isNaN(value)) {
      const contextStr = context ? ` in ${context}` : "";
      throw new ConfigurationError(`'${key}'${contextStr} must be a number`);
    }
    return value;
  }

  /**
   * Validates that a value is an array
   */
  protected validateArray<T>(
    value: unknown,
    key: string,
    context?: string,
  ): T[] {
    if (!Array.isArray(value)) {
      const contextStr = context ? ` in ${context}` : "";
      throw new ConfigurationError(`'${key}'${contextStr} must be an array`);
    }
    return value as T[];
  }

  /**
   * Validates that a value is an object
   */
  protected validateObject(
    value: unknown,
    key: string,
    context?: string,
  ): Record<string, unknown> {
    if (!value || typeof value !== "object" || Array.isArray(value)) {
      const contextStr = context ? ` in ${context}` : "";
      throw new ConfigurationError(`'${key}'${contextStr} must be an object`);
    }
    return value as Record<string, unknown>;
  }

  /**
   * Validates a port number
   */
  protected validatePort(
    value: unknown,
    key: string,
    context?: string,
  ): number {
    const port = this.validateNumber(value, key, context);
    if (port <= 0 || port > 65535) {
      const contextStr = context ? ` in ${context}` : "";
      throw new ConfigurationError(
        `'${key}'${contextStr} must be a valid port number (1-65535)`,
      );
    }
    return port;
  }

  /**
   * Validates a hostname or IP address (basic validation)
   */
  protected validateHost(
    value: unknown,
    key: string,
    context?: string,
  ): string {
    const host = this.validateString(value, key, context);
    if (!host.trim()) {
      const contextStr = context ? ` in ${context}` : "";
      throw new ConfigurationError(`'${key}'${contextStr} cannot be empty`);
    }
    return host.trim();
  }

  /**
   * Returns a copy of the raw configuration data
   */
  public getRawConfig(): Record<string, unknown> {
    return { ...this.raw };
  }
}

/**
 * Custom error class for configuration-related errors
 */
export class ConfigurationError extends Error {
  constructor(message: string, public readonly context?: string) {
    super(message);
    this.name = "ConfigurationError";
  }
}

/**
 * Interface for validatable configuration objects
 */
export interface Validatable {
  validate(): void;
}

/**
 * Type for configuration objects that can be serialized to environment-specific formats
 */
export interface Serializable {
  toObject(): Record<string, unknown>;
}
