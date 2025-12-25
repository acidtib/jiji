/**
 * Validation result interface
 */
export interface ValidationResult {
  valid: boolean;
  errors: ValidationError[];
  warnings: ValidationWarning[];
}

/**
 * Validation error interface
 */
export interface ValidationError {
  path: string;
  message: string;
  code?: string;
}

/**
 * Validation warning interface
 */
export interface ValidationWarning {
  path: string;
  message: string;
  code?: string;
}

/**
 * Validation rule interface
 */
export interface ValidationRule {
  name: string;
  validate(
    value: unknown,
    path: string,
    context: ValidationContext,
  ): ValidationResult;
}

/**
 * Validation context for passing additional information
 */
export interface ValidationContext {
  config: Record<string, unknown>;
  environment?: string;
  [key: string]: unknown;
}

/**
 * Configuration validator class
 */
export class ConfigurationValidator {
  private rules: Map<string, ValidationRule[]> = new Map();

  /**
   * Adds a validation rule for a specific path
   */
  addRule(path: string, rule: ValidationRule): void {
    if (!this.rules.has(path)) {
      this.rules.set(path, []);
    }
    this.rules.get(path)!.push(rule);
  }

  /**
   * Adds multiple rules for a specific path
   */
  addRules(path: string, rules: ValidationRule[]): void {
    for (const rule of rules) {
      this.addRule(path, rule);
    }
  }

  /**
   * Validates a configuration object
   */
  validate(
    config: Record<string, unknown>,
    context: ValidationContext = { config },
  ): ValidationResult {
    const errors: ValidationError[] = [];
    const warnings: ValidationWarning[] = [];

    // Update context with config
    context.config = config;

    // Validate each registered path
    for (const [path, rules] of this.rules) {
      const value = this.getValue(config, path);

      for (const rule of rules) {
        const result = rule.validate(value, path, context);
        errors.push(...result.errors);
        warnings.push(...result.warnings);
      }
    }

    return {
      valid: errors.length === 0,
      errors,
      warnings,
    };
  }

  /**
   * Gets a value from a nested object using dot notation
   */
  private getValue(obj: Record<string, unknown>, path: string): unknown {
    const parts = path.split(".");
    let current: unknown = obj;

    for (const part of parts) {
      if (current && typeof current === "object" && !Array.isArray(current)) {
        current = (current as Record<string, unknown>)[part];
      } else {
        return undefined;
      }
    }

    return current;
  }
}

/**
 * Built-in validation rules
 */
export class ValidationRules {
  /**
   * Rule to check if a value is required (not undefined/null)
   */
  static required(message?: string): ValidationRule {
    return {
      name: "required",
      validate(value: unknown, path: string): ValidationResult {
        const errors: ValidationError[] = [];

        if (value === undefined || value === null) {
          errors.push({
            path,
            message: message || `'${path}' is required`,
            code: "REQUIRED",
          });
        }

        return { valid: errors.length === 0, errors, warnings: [] };
      },
    };
  }

  /**
   * Rule to check if a value is a string
   */
  static string(message?: string): ValidationRule {
    return {
      name: "string",
      validate(value: unknown, path: string): ValidationResult {
        const errors: ValidationError[] = [];

        if (value !== undefined && typeof value !== "string") {
          errors.push({
            path,
            message: message || `'${path}' must be a string`,
            code: "TYPE_STRING",
          });
        }

        return { valid: errors.length === 0, errors, warnings: [] };
      },
    };
  }

  /**
   * Rule to check if a value is a number
   */
  static number(message?: string): ValidationRule {
    return {
      name: "number",
      validate(value: unknown, path: string): ValidationResult {
        const errors: ValidationError[] = [];

        if (
          value !== undefined && (typeof value !== "number" || isNaN(value))
        ) {
          errors.push({
            path,
            message: message || `'${path}' must be a number`,
            code: "TYPE_NUMBER",
          });
        }

        return { valid: errors.length === 0, errors, warnings: [] };
      },
    };
  }

  /**
   * Rule to check if a value is an array
   */
  static array(message?: string): ValidationRule {
    return {
      name: "array",
      validate(value: unknown, path: string): ValidationResult {
        const errors: ValidationError[] = [];

        if (value !== undefined && !Array.isArray(value)) {
          errors.push({
            path,
            message: message || `'${path}' must be an array`,
            code: "TYPE_ARRAY",
          });
        }

        return { valid: errors.length === 0, errors, warnings: [] };
      },
    };
  }

  /**
   * Rule to check if a value is an object
   */
  static object(message?: string): ValidationRule {
    return {
      name: "object",
      validate(value: unknown, path: string): ValidationResult {
        const errors: ValidationError[] = [];

        if (
          value !== undefined &&
          (!value || typeof value !== "object" || Array.isArray(value))
        ) {
          errors.push({
            path,
            message: message || `'${path}' must be an object`,
            code: "TYPE_OBJECT",
          });
        }

        return { valid: errors.length === 0, errors, warnings: [] };
      },
    };
  }

  /**
   * Rule to check if a value is one of allowed values
   */
  static oneOf<T>(
    allowedValues: readonly T[],
    message?: string,
  ): ValidationRule {
    return {
      name: "oneOf",
      validate(value: unknown, path: string): ValidationResult {
        const errors: ValidationError[] = [];

        if (value !== undefined && !allowedValues.includes(value as T)) {
          errors.push({
            path,
            message: message ||
              `'${path}' must be one of: ${allowedValues.join(", ")}`,
            code: "ENUM",
          });
        }

        return { valid: errors.length === 0, errors, warnings: [] };
      },
    };
  }

  /**
   * Rule to check minimum value for numbers
   */
  static min(minValue: number, message?: string): ValidationRule {
    return {
      name: "min",
      validate(value: unknown, path: string): ValidationResult {
        const errors: ValidationError[] = [];

        if (typeof value === "number" && value < minValue) {
          errors.push({
            path,
            message: message || `'${path}' must be at least ${minValue}`,
            code: "MIN_VALUE",
          });
        }

        return { valid: errors.length === 0, errors, warnings: [] };
      },
    };
  }

  /**
   * Rule to check maximum value for numbers
   */
  static max(maxValue: number, message?: string): ValidationRule {
    return {
      name: "max",
      validate(value: unknown, path: string): ValidationResult {
        const errors: ValidationError[] = [];

        if (typeof value === "number" && value > maxValue) {
          errors.push({
            path,
            message: message || `'${path}' must be at most ${maxValue}`,
            code: "MAX_VALUE",
          });
        }

        return { valid: errors.length === 0, errors, warnings: [] };
      },
    };
  }

  /**
   * Rule to check string length
   */
  static length(
    minLength?: number,
    maxLength?: number,
    message?: string,
  ): ValidationRule {
    return {
      name: "length",
      validate(value: unknown, path: string): ValidationResult {
        const errors: ValidationError[] = [];

        if (typeof value === "string") {
          if (minLength !== undefined && value.length < minLength) {
            errors.push({
              path,
              message: message ||
                `'${path}' must be at least ${minLength} characters long`,
              code: "MIN_LENGTH",
            });
          }

          if (maxLength !== undefined && value.length > maxLength) {
            errors.push({
              path,
              message: message ||
                `'${path}' must be at most ${maxLength} characters long`,
              code: "MAX_LENGTH",
            });
          }
        }

        return { valid: errors.length === 0, errors, warnings: [] };
      },
    };
  }

  /**
   * Rule to check regex pattern
   */
  static pattern(regex: RegExp, message?: string): ValidationRule {
    return {
      name: "pattern",
      validate(value: unknown, path: string): ValidationResult {
        const errors: ValidationError[] = [];

        if (typeof value === "string" && !regex.test(value)) {
          errors.push({
            path,
            message: message || `'${path}' format is invalid`,
            code: "PATTERN",
          });
        }

        return { valid: errors.length === 0, errors, warnings: [] };
      },
    };
  }

  /**
   * Rule to validate port numbers
   */
  static port(message?: string): ValidationRule {
    return {
      name: "port",
      validate(value: unknown, path: string): ValidationResult {
        const errors: ValidationError[] = [];

        if (typeof value === "number") {
          if (value <= 0 || value > 65535 || !Number.isInteger(value)) {
            errors.push({
              path,
              message: message ||
                `'${path}' must be a valid port number (1-65535)`,
              code: "INVALID_PORT",
            });
          }
        }

        return { valid: errors.length === 0, errors, warnings: [] };
      },
    };
  }

  /**
   * Rule to validate hostname/IP address format
   */
  static hostname(message?: string): ValidationRule {
    return {
      name: "hostname",
      validate(value: unknown, path: string): ValidationResult {
        const errors: ValidationError[] = [];
        const warnings: ValidationWarning[] = [];

        if (typeof value === "string") {
          const trimmed = value.trim();

          if (trimmed.length === 0) {
            errors.push({
              path,
              message: message || `'${path}' cannot be empty`,
              code: "EMPTY_HOSTNAME",
            });
          } else {
            // Basic hostname/IP validation
            const hostnameRegex =
              /^[a-zA-Z0-9]([a-zA-Z0-9\-]{0,61}[a-zA-Z0-9])?(\.[a-zA-Z0-9]([a-zA-Z0-9\-]{0,61}[a-zA-Z0-9])?)*$/;
            const ipv4Regex = /^((25[0-5]|(2[0-4]|1\d|[1-9]|)\d)\.?\b){4}$/;
            const ipv6Regex =
              /^(([0-9a-fA-F]{1,4}:){7,7}[0-9a-fA-F]{1,4}|([0-9a-fA-F]{1,4}:){1,7}:|([0-9a-fA-F]{1,4}:){1,6}:[0-9a-fA-F]{1,4}|([0-9a-fA-F]{1,4}:){1,5}(:[0-9a-fA-F]{1,4}){1,2}|([0-9a-fA-F]{1,4}:){1,4}(:[0-9a-fA-F]{1,4}){1,3}|([0-9a-fA-F]{1,4}:){1,3}(:[0-9a-fA-F]{1,4}){1,4}|([0-9a-fA-F]{1,4}:){1,2}(:[0-9a-fA-F]{1,4}){1,5}|[0-9a-fA-F]{1,4}:((:[0-9a-fA-F]{1,4}){1,6})|:((:[0-9a-fA-F]{1,4}){1,7}|:)|fe80:(:[0-9a-fA-F]{0,4}){0,4}%[0-9a-zA-Z]{1,}|::(ffff(:0{1,4}){0,1}:){0,1}((25[0-5]|(2[0-4]|1{0,1}[0-9]){0,1}[0-9])\.){3,3}(25[0-5]|(2[0-4]|1{0,1}[0-9]){0,1}[0-9])|([0-9a-fA-F]{1,4}:){1,4}:((25[0-5]|(2[0-4]|1{0,1}[0-9]){0,1}[0-9])\.){3,3}(25[0-5]|(2[0-4]|1{0,1}[0-9]){0,1}[0-9]))$/;

            if (
              !hostnameRegex.test(trimmed) && !ipv4Regex.test(trimmed) &&
              !ipv6Regex.test(trimmed)
            ) {
              errors.push({
                path,
                message: message ||
                  `'${path}' is not a valid hostname or IP address`,
                code: "INVALID_HOSTNAME",
              });
            }

            // Warning for localhost usage
            if (trimmed === "localhost" || trimmed === "127.0.0.1") {
              warnings.push({
                path,
                message:
                  `'${path}' uses localhost - this may not work in distributed deployments`,
                code: "LOCALHOST_WARNING",
              });
            }
          }
        }

        return { valid: errors.length === 0, errors, warnings };
      },
    };
  }

  /**
   * Custom validation rule
   */
  static custom(
    name: string,
    validator: (
      value: unknown,
      path: string,
      context: ValidationContext,
    ) => ValidationResult,
  ): ValidationRule {
    return {
      name,
      validate: validator,
    };
  }
}

/**
 * Pre-built validator configurations for common use cases
 */
export class ValidatorPresets {
  /**
   * Creates a validator for the main jiji configuration
   */
  static createJijiValidator(): ConfigurationValidator {
    const validator = new ConfigurationValidator();

    // Project validation
    validator.addRules("project", [
      ValidationRules.required(),
      ValidationRules.string(),
      ValidationRules.length(1, 50, "Project name must be 1-50 characters"),
      ValidationRules.pattern(
        /^[a-z0-9]+([_-][a-z0-9]+)*$/,
        "Project name must contain only lowercase letters, numbers, hyphens, and underscores",
      ),
    ]);

    // SSH configuration
    validator.addRules("ssh.user", [
      ValidationRules.required(),
      ValidationRules.string(),
      ValidationRules.length(1),
    ]);

    validator.addRules("ssh.port", [
      ValidationRules.number(),
      ValidationRules.port(),
    ]);

    // SSH Proxy validation
    validator.addRule("ssh.proxy", ValidationRules.string());
    validator.addRule(
      "ssh.proxy",
      ValidationRules.pattern(
        /^(?:([^@]+)@)?([^:]+)(?::(\d+))?$/,
        "Proxy must be in format: [user@]hostname[:port]",
      ),
    );

    validator.addRule("ssh.proxy_command", ValidationRules.string());
    validator.addRule(
      "ssh.proxy_command",
      ValidationRules.custom(
        "proxy_command_placeholders",
        (value, path) => {
          const errors: ValidationError[] = [];

          if (typeof value === "string") {
            if (!value.includes("%h") || !value.includes("%p")) {
              errors.push({
                path,
                message: "proxy_command must contain %h and %p placeholders",
                code: "MISSING_PLACEHOLDERS",
              });
            }
          }

          return { valid: errors.length === 0, errors, warnings: [] };
        },
      ),
    );

    // Mutual exclusivity check for proxy and proxy_command
    validator.addRule(
      "ssh.proxy",
      ValidationRules.custom(
        "proxy_mutual_exclusivity",
        (value, _path, context) => {
          const errors: ValidationError[] = [];
          const sshConfig = context.config.ssh as Record<string, unknown>;

          if (value && sshConfig?.proxy_command) {
            errors.push({
              path: "ssh.proxy",
              message:
                "Cannot specify both 'proxy' and 'proxy_command' - use only one",
              code: "PROXY_MUTUAL_EXCLUSIVITY",
            });
          }

          return { valid: errors.length === 0, errors, warnings: [] };
        },
      ),
    );

    // Builder validation
    validator.addRules("builder", [
      ValidationRules.required(),
      ValidationRules.object(),
    ]);

    validator.addRules("builder.engine", [
      ValidationRules.required(),
      ValidationRules.string(),
      ValidationRules.oneOf(["docker", "podman"] as const),
    ]);

    // Services validation
    validator.addRules("services", [
      ValidationRules.required(),
      ValidationRules.object(),
    ]);

    return validator;
  }

  /**
   * Creates a validator for service configurations
   */
  static createServiceValidator(): ConfigurationValidator {
    const validator = new ConfigurationValidator();

    // Image or build required
    validator.addRule(
      "image",
      ValidationRules.custom("image_or_build", (value, _path, context) => {
        const errors: ValidationError[] = [];
        const config = context.config as Record<string, unknown>;

        if (!value && !config.build) {
          errors.push({
            path: "image",
            message: "Either image or build must be specified",
            code: "IMAGE_OR_BUILD_REQUIRED",
          });
        }

        return { valid: errors.length === 0, errors, warnings: [] };
      }),
    );

    // Servers validation
    validator.addRules("servers", [
      ValidationRules.required(),
      ValidationRules.array(),
    ]);

    // Port validation
    validator.addRule("ports", ValidationRules.array());

    return validator;
  }
}
