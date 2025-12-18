import { ConfigurationLoader } from "./configuration/loader.ts";
import { SSHConfiguration } from "./configuration/ssh.ts";
import { ServiceConfiguration } from "./configuration/service.ts";
import { EnvironmentConfiguration } from "./configuration/environment.ts";
import { ValidatorPresets } from "./configuration/validation.ts";
import type { ValidationResult } from "./configuration/validation.ts";
import { BaseConfiguration, ConfigurationError } from "./configuration/base.ts";
import { log } from "../utils/logger.ts";

export type ContainerEngine = "docker" | "podman";

/**
 * Main configuration class orchestrating all jiji configuration aspects
 */
export class Configuration extends BaseConfiguration {
  private _project?: string;
  private _engine?: ContainerEngine;
  private _ssh?: SSHConfiguration;
  private _services?: Map<string, ServiceConfiguration>;
  private _environment?: EnvironmentConfiguration;
  private _configPath?: string;
  private _environmentName?: string;

  constructor(
    config: Record<string, unknown> = {},
    configPath?: string,
    environment?: string,
  ) {
    super(config);
    this._configPath = configPath;
    this._environmentName = environment;
  }

  /**
   * Project name for organizing services
   */
  get project(): string {
    if (!this._project) {
      this._project = this.getRequired<string>("project");
      this.validateString(this._project, "project");
    }
    return this._project;
  }

  /**
   * Container engine to use (docker or podman)
   */
  get engine(): ContainerEngine {
    if (!this._engine) {
      this._engine = this.getRequired<ContainerEngine>("engine");
      this.validateEnum(
        this._engine,
        ["docker", "podman"] as const,
        "engine",
      );
    }
    return this._engine;
  }

  /**
   * SSH configuration for remote connections
   */
  get ssh(): SSHConfiguration {
    if (!this._ssh) {
      const sshConfig = this.has("ssh") ? this.get("ssh") : {};
      this._ssh = new SSHConfiguration(
        this.validateObject(sshConfig, "ssh"),
      );
    }
    return this._ssh;
  }

  /**
   * Services configuration map
   */
  get services(): Map<string, ServiceConfiguration> {
    if (!this._services) {
      this._services = new Map();
      const servicesConfig = this.getRequired<Record<string, unknown>>(
        "services",
      );
      this.validateObject(servicesConfig, "services");

      for (const [name, config] of Object.entries(servicesConfig)) {
        const serviceConfig = this.validateObject(
          config,
          `services.${name}`,
        );
        this._services.set(
          name,
          new ServiceConfiguration(name, serviceConfig, this.project),
        );
      }
    }
    return this._services;
  }

  /**
   * Environment configuration
   */
  get environment(): EnvironmentConfiguration {
    if (!this._environment) {
      const envConfig = this.has("env") ? this.get("env") : {};
      const envName = this._environmentName || "default";
      this._environment = new EnvironmentConfiguration(
        envName,
        this.validateObject(envConfig, "env"),
      );
    }
    return this._environment;
  }

  /**
   * Configuration file path
   */
  get configPath(): string | undefined {
    return this._configPath;
  }

  /**
   * Environment name
   */
  get environmentName(): string | undefined {
    return this._environmentName;
  }

  /**
   * Gets a specific service configuration by name
   */
  getService(name: string): ServiceConfiguration {
    const service = this.services.get(name);
    if (!service) {
      throw new ConfigurationError(`Service '${name}' not found`);
    }
    return service;
  }

  /**
   * Gets all service names
   */
  getServiceNames(): string[] {
    return Array.from(this.services.keys());
  }

  /**
   * Checks if a service exists
   */
  hasService(name: string): boolean {
    return this.services.has(name);
  }

  /**
   * Gets services filtered by host
   */
  getServicesForHost(hostname: string): ServiceConfiguration[] {
    return Array.from(this.services.values()).filter((service) =>
      service.hosts.includes(hostname)
    );
  }

  /**
   * Gets all unique hosts from all services
   */
  getAllHosts(): string[] {
    const hosts = new Set<string>();
    for (const service of this.services.values()) {
      for (const host of service.hosts) {
        hosts.add(host);
      }
    }
    return Array.from(hosts).sort();
  }

  /**
   * Gets hosts from specific services
   * Supports wildcards with * pattern matching
   *
   * @param serviceNames - Array of service names (supports wildcards)
   * @returns Array of unique hosts from matching services
   */
  getHostsFromServices(serviceNames: string[]): string[] {
    const hosts = new Set<string>();
    const allServiceNames = this.getServiceNames();

    for (const pattern of serviceNames) {
      // Support wildcard patterns
      const matchingServices = this.matchServicePattern(
        pattern,
        allServiceNames,
      );

      for (const serviceName of matchingServices) {
        const service = this.services.get(serviceName);
        if (service) {
          service.hosts.forEach((host) => hosts.add(host));
        }
      }
    }

    return Array.from(hosts).sort();
  }

  /**
   * Match service names against a pattern (supports wildcards)
   *
   * @param pattern - Pattern to match (supports * and ? wildcards)
   * @param serviceNames - Array of service names to match against
   * @returns Array of matching service names
   */
  private matchServicePattern(
    pattern: string,
    serviceNames: string[],
  ): string[] {
    // Exact match first
    if (serviceNames.includes(pattern)) {
      return [pattern];
    }

    // Wildcard matching (supports * and ?)
    if (pattern.includes("*") || pattern.includes("?")) {
      const regex = new RegExp(
        "^" + pattern.replace(/\*/g, ".*").replace(/\?/g, ".") + "$",
      );
      return serviceNames.filter((name) => regex.test(name));
    }

    // No match
    return [];
  }

  /**
   * Gets matching service names based on pattern (supports wildcards)
   *
   * @param patterns - Array of patterns to match
   * @returns Array of matching service names
   */
  getMatchingServiceNames(patterns: string[]): string[] {
    const allServiceNames = this.getServiceNames();
    const matchingNames = new Set<string>();

    for (const pattern of patterns) {
      const matches = this.matchServicePattern(pattern, allServiceNames);
      matches.forEach((name) => matchingNames.add(name));
    }

    return Array.from(matchingNames).sort();
  }

  /**
   * Gets services that require building
   */
  getBuildServices(): ServiceConfiguration[] {
    return Array.from(this.services.values()).filter((service) =>
      service.requiresBuild()
    );
  }

  /**
   * Validates the entire configuration
   */
  validate(): ValidationResult {
    const validator = ValidatorPresets.createJijiValidator();
    const result = validator.validate(this.raw, {
      config: this.raw,
      environment: this._environmentName,
    });

    // Validate SSH configuration
    try {
      this.ssh.validate();
    } catch (error) {
      if (error instanceof ConfigurationError) {
        result.errors.push({
          path: "ssh",
          message: error.message,
          code: "SSH_VALIDATION",
        });
        result.valid = false;
      }
    }

    // Validate each service
    for (const service of this.services.values()) {
      try {
        service.validate();
      } catch (error) {
        if (error instanceof ConfigurationError) {
          result.errors.push({
            path: `services.${service.name}`,
            message: error.message,
            code: "SERVICE_VALIDATION",
          });
          result.valid = false;
        }
      }
    }

    // Validate environment
    try {
      this.environment.validate();
    } catch (error) {
      if (error instanceof ConfigurationError) {
        result.errors.push({
          path: "env",
          message: error.message,
          code: "ENVIRONMENT_VALIDATION",
        });
        result.valid = false;
      }
    }

    // Custom cross-service validations
    this.validateHostConsistency(result);

    return result;
  }

  /**
   * Validates host consistency across services
   */
  private validateHostConsistency(result: ValidationResult): void {
    const allHosts = this.getAllHosts();

    for (const service of this.services.values()) {
      if (service.hosts.length === 0) {
        result.errors.push({
          path: `services.${service.name}.hosts`,
          message: `Service '${service.name}' must specify at least one host`,
          code: "NO_HOSTS",
        });
        result.valid = false;
      }
    }

    // Warn about unused hosts in environment
    if (allHosts.length > 10) {
      result.warnings.push({
        path: "services",
        message:
          `Large number of hosts (${allHosts.length}) may impact deployment performance`,
        code: "MANY_HOSTS",
      });
    }
  }

  /**
   * Returns the configuration as a plain object
   */
  toObject(): Record<string, unknown> {
    const result: Record<string, unknown> = {
      project: this.project,
      engine: this.engine,
    };

    // Add SSH config if present
    const sshObj = this.ssh.toObject();
    if (Object.keys(sshObj).length > 0) {
      result.ssh = sshObj;
    }

    // Add services
    const servicesObj: Record<string, unknown> = {};
    for (const [name, service] of this.services) {
      servicesObj[name] = service.toObject();
    }
    result.services = servicesObj;

    // Add environment if present
    const envObj = this.environment.toObject();
    if (Object.keys(envObj).length > 0) {
      result.env = envObj;
    }

    return result;
  }

  /**
   * Loads configuration from file system
   */
  static async load(
    environment?: string,
    configPath?: string,
    startPath?: string,
  ): Promise<Configuration> {
    const { config, path } = await ConfigurationLoader.loadConfig(
      environment,
      configPath,
      startPath,
    );

    const configuration = new Configuration(config, path, environment);

    // Validate the loaded configuration
    const validationResult = configuration.validate();
    if (!validationResult.valid) {
      const errorMessages = validationResult.errors
        .map((err) => `${err.path}: ${err.message}`)
        .join("\n");

      throw new ConfigurationError(
        `Configuration validation failed:\n${errorMessages}`,
        path,
      );
    }

    // Log warnings if any
    if (validationResult.warnings.length > 0) {
      const warningMessages = validationResult.warnings
        .map((warn) => `${warn.path}: ${warn.message}`)
        .join("\n");

      log.warn(`Configuration warnings:\n${warningMessages}`, "config");
    }

    return configuration;
  }

  /**
   * Creates a configuration with defaults for testing or initialization
   */
  static withDefaults(overrides: Record<string, unknown> = {}): Configuration {
    const defaultConfig = {
      project: "default",
      engine: "podman",
      ssh: {
        user: "root",
        port: 22,
      },
      services: {
        web: {
          image: "nginx:latest",
          hosts: ["localhost"],
          ports: ["80:80"],
        },
      },
      ...overrides,
    };

    return new Configuration(defaultConfig);
  }

  /**
   * Gets available configuration files in current directory
   */
  static async getAvailableConfigs(
    searchPath?: string,
  ): Promise<string[]> {
    return await ConfigurationLoader.getAvailableConfigs(searchPath);
  }

  /**
   * Validates a configuration file without loading it completely
   */
  static async validateFile(configPath: string): Promise<ValidationResult> {
    await ConfigurationLoader.validateConfigPath(configPath);
    const config = await ConfigurationLoader.loadFromFile(configPath);
    const configuration = new Configuration(config, configPath);
    return configuration.validate();
  }
}

// Re-export commonly used types and classes
export { ConfigurationError } from "./configuration/base.ts";
export { SSHConfiguration } from "./configuration/ssh.ts";
export {
  type BuildConfig,
  ServiceConfiguration,
} from "./configuration/service.ts";
export { EnvironmentConfiguration } from "./configuration/environment.ts";
export { ConfigurationLoader } from "./configuration/loader.ts";
export {
  ProxyConfiguration,
  type ProxyHealthcheckConfig,
} from "./configuration/proxy.ts";
export {
  ConfigurationValidator,
  type ValidationError,
  type ValidationResult,
  ValidationRules,
  type ValidationWarning,
  ValidatorPresets,
} from "./configuration/validation.ts";
