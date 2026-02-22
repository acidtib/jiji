import { dirname } from "@std/path";
import { ConfigurationLoader } from "./configuration/loader.ts";
import { SSHConfiguration } from "./configuration/ssh.ts";
import { ServiceConfiguration } from "./configuration/service.ts";
import { EnvironmentConfiguration } from "./configuration/environment.ts";
import { BuilderConfiguration } from "./configuration/builder.ts";
import { NetworkConfiguration } from "./configuration/network.ts";
import {
  type ResolvedServerConfig,
  ServersConfiguration,
} from "./configuration/servers.ts";
import { SecretsConfiguration } from "./configuration/secrets.ts";
import { ValidatorPresets } from "./configuration/validation.ts";
import type { ValidationResult } from "./configuration/validation.ts";
import { BaseConfiguration, ConfigurationError } from "./configuration/base.ts";
import { log } from "../utils/logger.ts";
import { DEFAULT_LOCAL_REGISTRY_PORT } from "../constants.ts";
import { resolveSecrets } from "../utils/secret_resolver.ts";

export type ContainerEngine = "docker" | "podman";

/**
 * Main configuration class orchestrating all jiji configuration aspects
 */
export class Configuration extends BaseConfiguration {
  private _project?: string;
  private _ssh?: SSHConfiguration;
  private _servers?: ServersConfiguration;
  private _services?: Map<string, ServiceConfiguration>;
  private _environment?: EnvironmentConfiguration;
  private _builder?: BuilderConfiguration; // Lazy-loaded, required field
  private _network?: NetworkConfiguration;
  private _secrets?: SecretsConfiguration;
  private _configPath?: string;
  private _environmentName?: string;
  private _secretsPath?: string;

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
   * Servers configuration (top-level named servers)
   */
  get servers(): ServersConfiguration {
    if (!this._servers) {
      const serversConfig = this.getRequired<Record<string, unknown>>(
        "servers",
      );
      this._servers = new ServersConfiguration(
        this.validateObject(serversConfig, "servers"),
      );
    }
    return this._servers;
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

      // Get shared environment to pass to services
      const sharedEnv = this.environment;

      for (const [name, config] of Object.entries(servicesConfig)) {
        const serviceConfig = this.validateObject(
          config,
          `services.${name}`,
        );
        this._services.set(
          name,
          new ServiceConfiguration(
            name,
            serviceConfig,
            this.project,
            sharedEnv,
          ),
        );
      }
    }
    return this._services;
  }

  /**
   * Shared environment configuration
   */
  get environment(): EnvironmentConfiguration {
    if (!this._environment) {
      const envConfig = this.has("environment") ? this.get("environment") : {};
      this._environment = new EnvironmentConfiguration(
        this.validateObject(envConfig, "environment"),
      );
    }
    return this._environment;
  }

  /**
   * Builder configuration for building and managing container images
   */
  get builder(): BuilderConfiguration {
    if (!this._builder) {
      const builderConfig = this.getRequired<Record<string, unknown>>(
        "builder",
      );
      this._builder = new BuilderConfiguration(
        this.validateObject(builderConfig, "builder"),
      );
    }
    return this._builder;
  }

  /**
   * Network configuration for private networking
   */
  get network(): NetworkConfiguration {
    if (!this._network) {
      const networkConfig = this.has("network") ? this.get("network") : {};
      this._network = new NetworkConfiguration(
        this.validateObject(networkConfig, "network"),
      );
    }
    return this._network;
  }

  /**
   * Secrets adapter configuration (e.g., Doppler)
   */
  get secretsAdapter(): SecretsConfiguration {
    if (!this._secrets) {
      const secretsConfig = this.has("secrets") ? this.get("secrets") : {};
      this._secrets = new SecretsConfiguration(
        typeof secretsConfig === "object" && secretsConfig !== null &&
          !Array.isArray(secretsConfig)
          ? (secretsConfig as Record<string, unknown>)
          : {},
      );
    }
    return this._secrets;
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
   * Custom path to secrets .env file (relative to project root)
   * If not specified, defaults to .env.{environment} or .env
   */
  get secretsPath(): string | undefined {
    if (!this._secretsPath) {
      if (this.has("secrets_path")) {
        this._secretsPath = this.validateString(
          this.get("secrets_path"),
          "secrets_path",
        );
      }
    }
    return this._secretsPath;
  }

  /**
   * Gets the project root directory (parent of .jiji folder)
   */
  getProjectRoot(): string {
    if (this._configPath) {
      // Config is at .jiji/deploy.yml, so go up two levels
      return dirname(dirname(this._configPath));
    }
    return Deno.cwd();
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
   * Resolve a server name to full configuration with merged SSH settings
   */
  resolveServer(name: string): ResolvedServerConfig {
    const serverConfig = this.servers.getServer(name);
    if (!serverConfig) {
      throw new ConfigurationError(
        `Server '${name}' not found in servers section. ` +
          `Available servers: ${this.servers.getAllServerNames().join(", ")}`,
      );
    }

    const globalSSH = this.ssh;

    return {
      name,
      host: serverConfig.host,
      arch: serverConfig.arch || "amd64", // Default architecture
      ssh: {
        user: serverConfig.user || globalSSH.user,
        port: serverConfig.port || globalSSH.port,
        key_path: serverConfig.key_path || globalSSH.keyPath,
        key_passphrase: serverConfig.key_passphrase ||
          globalSSH.keyPassphrase,
        keys: serverConfig.keys || globalSSH.keys,
        key_data: serverConfig.key_data || globalSSH.keyData,
      },
    };
  }

  /**
   * Get all defined servers from the servers section
   * This returns ALL servers, even if not used by any service
   * Used for network initialization where all servers need to peer
   */
  getAllDefinedServers(): ResolvedServerConfig[] {
    const serverNames = Array.from(this.servers.servers.keys());
    return serverNames.map((name: string) => this.resolveServer(name));
  }

  /**
   * Get all resolved servers from all services (unique by name)
   * This returns only servers that are referenced by at least one service
   */
  getAllResolvedServers(): ResolvedServerConfig[] {
    const serverNames = new Set<string>();

    // Collect all server names referenced by services
    for (const service of this.services.values()) {
      for (const serverName of service.hosts) {
        serverNames.add(serverName);
      }
    }

    // Resolve each unique server name
    return Array.from(serverNames).map((name) => this.resolveServer(name));
  }

  /**
   * Get resolved servers for a specific service
   *
   * @param serviceName Name of the service
   * @returns Array of resolved server configurations for the service
   */
  getResolvedServersForService(
    serviceName: string,
  ): ResolvedServerConfig[] {
    const service = this.services.get(serviceName);
    if (!service) {
      throw new ConfigurationError(`Service '${serviceName}' not found`);
    }

    return service.hosts.map((serverName) => this.resolveServer(serverName));
  }

  /**
   * Get required architectures for a service
   *
   * @param serviceName Name of the service
   * @returns Array of unique architectures required by the service's servers
   */
  getRequiredArchitecturesForService(serviceName: string): string[] {
    const resolvedServers = this.getResolvedServersForService(serviceName);
    const archs = new Set<string>();

    for (const server of resolvedServers) {
      archs.add(server.arch);
    }

    return Array.from(archs).sort();
  }

  /**
   * Get servers grouped by architecture for a service
   *
   * @param serviceName Name of the service
   * @returns Map of architecture to resolved server configs
   */
  getServersByArchitectureForService(
    serviceName: string,
  ): Map<string, ResolvedServerConfig[]> {
    const resolvedServers = this.getResolvedServersForService(serviceName);
    const byArch = new Map<string, ResolvedServerConfig[]>();

    for (const server of resolvedServers) {
      if (!byArch.has(server.arch)) {
        byArch.set(server.arch, []);
      }
      byArch.get(server.arch)!.push(server);
    }

    return byArch;
  }

  /**
   * Gets services filtered by hostname
   */
  getServicesForHost(hostname: string): ServiceConfiguration[] {
    return Array.from(this.services.values()).filter((service) => {
      // Check if any of the service's server names resolve to this hostname
      return service.hosts.some((serverName) => {
        const resolved = this.resolveServer(serverName);
        return resolved.host === hostname;
      });
    });
  }

  /**
   * Gets all unique server hosts from all services
   */
  getAllServerHosts(): string[] {
    const hosts = new Set<string>();
    for (const service of this.services.values()) {
      for (const serverName of service.hosts) {
        const resolved = this.resolveServer(serverName);
        hosts.add(resolved.host);
      }
    }
    return Array.from(hosts).sort();
  }

  /**
   * Gets hostnames from specific services
   * Supports wildcards with * pattern matching
   *
   * @param serviceNames - Array of service names (supports wildcards)
   * @returns Array of unique hostnames from matching services
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
          service.hosts.forEach((serverName) => {
            const resolved = this.resolveServer(serverName);
            hosts.add(resolved.host);
          });
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
   * Gets all services that should be deployed (both build and image-based)
   */
  getDeployableServices(): ServiceConfiguration[] {
    return Array.from(this.services.values());
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
          path: "environment",
          message: error.message,
          code: "ENVIRONMENT_VALIDATION",
        });
        result.valid = false;
      }
    }

    // Validate builder configuration (required)
    try {
      this.builder.validate();
    } catch (error) {
      if (error instanceof ConfigurationError) {
        result.errors.push({
          path: "builder",
          message: error.message,
          code: "BUILDER_VALIDATION",
        });
        result.valid = false;
      }
    }

    // Validate network configuration
    try {
      this.network.validate();
    } catch (error) {
      if (error instanceof ConfigurationError) {
        result.errors.push({
          path: "network",
          message: error.message,
          code: "NETWORK_VALIDATION",
        });
        result.valid = false;
      }
    }

    // Validate secrets adapter configuration
    try {
      this.secretsAdapter.validate();
    } catch (error) {
      if (error instanceof ConfigurationError) {
        result.errors.push({
          path: "secrets",
          message: error.message,
          code: "SECRETS_VALIDATION",
        });
        result.valid = false;
      }
    }

    // Validate servers configuration (required)
    try {
      const serversResult = this.servers.validate();
      if (!serversResult.valid) {
        result.errors.push(...serversResult.errors);
        result.valid = false;
      }
      result.warnings.push(...serversResult.warnings);
    } catch (error) {
      if (error instanceof ConfigurationError) {
        result.errors.push({
          path: "servers",
          message: error.message,
          code: "SERVERS_VALIDATION",
        });
        result.valid = false;
      }
    }

    // Custom cross-service validations
    this.validateServerReferences(result);
    this.validateHostConsistency(result);

    return result;
  }

  /**
   * Validates server references in services
   */
  private validateServerReferences(result: ValidationResult): void {
    const definedServers = this.servers.getAllServerNames();

    for (const service of this.services.values()) {
      // Check that service has at least one host
      if (service.hosts.length === 0) {
        result.errors.push({
          path: `services.${service.name}.hosts`,
          message:
            `Service '${service.name}' must specify at least one server in 'hosts' array`,
          code: "NO_HOSTS",
        });
        result.valid = false;
      }

      // Check that all referenced servers exist
      for (const serverName of service.hosts) {
        if (!definedServers.includes(serverName)) {
          result.errors.push({
            path: `services.${service.name}.hosts`,
            message: `Server '${serverName}' not found in servers section. ` +
              `Available servers: ${definedServers.join(", ")}`,
            code: "UNDEFINED_SERVER",
          });
          result.valid = false;
        }
      }
    }
  }

  /**
   * Validates host consistency across services
   */
  private validateHostConsistency(result: ValidationResult): void {
    const allHosts = this.getAllServerHosts();

    // Warn about large number of hosts
    if (allHosts.length > 10) {
      result.warnings.push({
        path: "services",
        message:
          `Large number of hosts (${allHosts.length}) may impact deployment performance`,
        code: "MANY_HOSTS",
      });
    }

    // Warn about unused servers
    const definedServers = this.servers.getAllServerNames();
    const usedServers = new Set<string>();
    for (const service of this.services.values()) {
      service.hosts.forEach((name) => usedServers.add(name));
    }

    for (const serverName of definedServers) {
      if (!usedServers.has(serverName)) {
        result.warnings.push({
          path: `servers.${serverName}`,
          message:
            `Server '${serverName}' is defined but not used by any service`,
          code: "UNUSED_SERVER",
        });
      }
    }
  }

  /**
   * Returns the configuration as a plain object
   */
  toObject(): Record<string, unknown> {
    const result: Record<string, unknown> = {
      project: this.project,
    };

    // Add SSH config if present
    const sshObj = this.ssh.toObject();
    if (Object.keys(sshObj).length > 0) {
      result.ssh = sshObj;
    }

    // Add servers (required)
    const serversObj: Record<string, unknown> = {};
    for (const [name, server] of this.servers.servers) {
      serversObj[name] = server;
    }
    result.servers = serversObj;

    // Add services
    const servicesObj: Record<string, unknown> = {};
    for (const [name, service] of this.services) {
      servicesObj[name] = service.toObject();
    }
    result.services = servicesObj;

    // Add environment if present
    const envObj = this.environment.toObject();
    if (Object.keys(envObj).length > 0) {
      result.environment = envObj;
    }

    // Add builder (required)
    result.builder = this.builder.getRawConfig();

    // Add network if present
    const networkObj = this.network.toObject();
    if (Object.keys(networkObj).length > 0) {
      result.network = networkObj;
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

    // Load env vars for server host resolution (from .env + optional adapter)
    const projectRoot = configuration.getProjectRoot();
    const secretResult = await resolveSecrets(
      {
        projectRoot,
        environment,
        envPath: configuration.secretsPath,
      },
      configuration.secretsAdapter,
    );
    configuration.servers.setEnvVars(secretResult.variables);

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
      ssh: {
        user: "root",
        port: 22,
      },
      builder: {
        engine: "podman",
        local: true,
        registry: {
          type: "local",
          port: DEFAULT_LOCAL_REGISTRY_PORT,
        },
      },
      services: {
        web: {
          image: "nginx:latest",
          servers: [{ host: "localhost" }],
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

// Re export commonly used types and classes
export { ConfigurationError } from "./configuration/base.ts";
export { SSHConfiguration } from "./configuration/ssh.ts";
export {
  type BuildConfig,
  ServiceConfiguration,
} from "./configuration/service.ts";
export { EnvironmentConfiguration } from "./configuration/environment.ts";
export { BuilderConfiguration } from "./configuration/builder.ts";
export { NetworkConfiguration } from "./configuration/network.ts";
export { RegistryConfiguration } from "./configuration/registry.ts";
export { ConfigurationLoader } from "./configuration/loader.ts";
export {
  ProxyConfiguration,
  type ProxyHealthcheckConfig,
} from "./configuration/proxy.ts";
export { SecretsConfiguration } from "./configuration/secrets.ts";
export {
  ConfigurationValidator,
  type ValidationError,
  type ValidationResult,
  ValidationRules,
  type ValidationWarning,
  ValidatorPresets,
} from "./configuration/validation.ts";
