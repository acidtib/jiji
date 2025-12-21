import { BaseConfiguration, ConfigurationError } from "./base.ts";
import { log } from "../../utils/logger.ts";

/**
 * Interface for proxy healthcheck configuration
 */
export interface ProxyHealthcheckConfig {
  path?: string;
  interval?: string;
  timeout?: string;
  deploy_timeout?: string;
}

/**
 * Modern ProxyConfiguration for kamal-proxy
 * Supports host-based and path-based routing with SSL termination
 */
export class ProxyConfiguration extends BaseConfiguration {
  private rawConfig: Record<string, unknown>;

  // Validation patterns
  private static readonly HOST_PATTERN =
    /^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)*$/i;
  private static readonly INVALID_PATH_CHARS = /[<>"|?*]/;
  private static readonly INTERVAL_PATTERN = /^\d+[smh]$/;

  constructor(config: Record<string, unknown>) {
    super(config);
    this.rawConfig = config;
  }

  get ssl(): boolean {
    const value = this.get<unknown>("ssl", false);
    return typeof value === "boolean" ? value : false;
  }

  get host(): string | undefined {
    const value = this.get<unknown>("host");
    return typeof value === "string" ? value : undefined;
  }

  get hosts(): string[] {
    const hostsValue = this.get<unknown>("hosts");
    const hostValue = this.get<unknown>("host");

    if (Array.isArray(hostsValue)) {
      return hostsValue.filter((h): h is string => typeof h === "string");
    }

    if (typeof hostValue === "string") {
      return [hostValue];
    }

    return [];
  }

  get pathPrefix(): string | undefined {
    const value = this.get<unknown>("path_prefix");
    return typeof value === "string" ? value : undefined;
  }

  get healthcheck(): ProxyHealthcheckConfig | undefined {
    const healthcheckConfig = this.rawConfig.healthcheck;
    if (!healthcheckConfig || typeof healthcheckConfig !== "object") {
      return undefined;
    }

    const config = healthcheckConfig as Record<string, unknown>;
    return {
      path: typeof config.path === "string" ? config.path : undefined,
      interval: typeof config.interval === "string"
        ? config.interval
        : undefined,
      timeout: typeof config.timeout === "string" ? config.timeout : undefined,
      deploy_timeout: typeof config.deploy_timeout === "string"
        ? config.deploy_timeout
        : undefined,
    };
  }

  get enabled(): boolean {
    return this.hosts.length > 0;
  }

  validate(): void {
    const errors: ConfigurationError[] = [];
    const warnings: string[] = [];

    const hasHost = this.rawConfig.host !== undefined;
    const hasHosts = this.rawConfig.hosts !== undefined;

    if (hasHost && hasHosts) {
      errors.push(
        new ConfigurationError(
          "Use either 'host' or 'hosts', not both in proxy configuration",
        ),
      );
    }

    for (const host of this.hosts) {
      if (!ProxyConfiguration.HOST_PATTERN.test(host)) {
        errors.push(
          new ConfigurationError(`Invalid host format: ${host}`),
        );
      }

      if (host === "localhost" || host === "127.0.0.1") {
        warnings.push(
          `Host '${host}' uses localhost - this may not work in distributed deployments`,
        );
      }
    }

    if (this.pathPrefix) {
      if (!this.pathPrefix.startsWith("/")) {
        errors.push(
          new ConfigurationError(
            `Path prefix must start with '/': ${this.pathPrefix}`,
          ),
        );
      }

      if (ProxyConfiguration.INVALID_PATH_CHARS.test(this.pathPrefix)) {
        errors.push(
          new ConfigurationError(
            `Path prefix contains invalid characters: ${this.pathPrefix}`,
          ),
        );
      }

      if (this.pathPrefix !== "/" && this.pathPrefix.endsWith("/")) {
        warnings.push(
          `Path prefix '${this.pathPrefix}' has trailing slash - this may affect routing behavior`,
        );
      }
    }

    // Health check validation
    const healthcheck = this.healthcheck;
    if (healthcheck?.path) {
      if (!healthcheck.path.startsWith("/")) {
        errors.push(
          new ConfigurationError(
            `Health check path must start with /: ${healthcheck.path}`,
          ),
        );
      }
      if (ProxyConfiguration.INVALID_PATH_CHARS.test(healthcheck.path)) {
        errors.push(
          new ConfigurationError(
            `Health check path contains invalid characters: ${healthcheck.path}`,
          ),
        );
      }
    }

    if (healthcheck?.interval) {
      if (!ProxyConfiguration.INTERVAL_PATTERN.test(healthcheck.interval)) {
        errors.push(
          new ConfigurationError(
            `Invalid health check interval: ${healthcheck.interval}`,
          ),
        );
      }
      // Check for minimum interval
      const match = healthcheck.interval.match(/^(\d+)([smh])$/);
      if (match) {
        const value = parseInt(match[1]);
        const unit = match[2];
        if (unit === "s" && value < 1) {
          errors.push(
            new ConfigurationError(
              `Health check interval too short: ${healthcheck.interval}. Minimum is 1s.`,
            ),
          );
        }
      }
    }

    // Timeout validation
    if (healthcheck?.timeout) {
      if (!ProxyConfiguration.INTERVAL_PATTERN.test(healthcheck.timeout)) {
        errors.push(
          new ConfigurationError(
            `Invalid health check timeout: ${healthcheck.timeout}`,
          ),
        );
      }
    }

    if (healthcheck?.deploy_timeout) {
      if (
        !ProxyConfiguration.INTERVAL_PATTERN.test(healthcheck.deploy_timeout)
      ) {
        errors.push(
          new ConfigurationError(
            `Invalid deploy timeout: ${healthcheck.deploy_timeout}`,
          ),
        );
      }
    }

    // SSL requires host
    if (this.ssl && this.hosts.length === 0) {
      errors.push(
        new ConfigurationError(
          "SSL requires at least one host to be configured",
        ),
      );
    }

    // Throw first error if any exist
    if (errors.length > 0) {
      throw errors[0];
    }

    // Log warnings
    warnings.forEach((w) => log.warn(w, "proxy"));
  }
}
