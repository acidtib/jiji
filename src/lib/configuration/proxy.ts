import { BaseConfiguration } from "./base.ts";

/**
 * Interface for proxy healthcheck configuration
 */
export interface ProxyHealthcheckConfig {
  path?: string;
  interval?: string;
}

/**
 * Modern ProxyConfiguration for kamal-proxy
 * Supports host-based and path-based routing with SSL termination
 */
export class ProxyConfiguration extends BaseConfiguration {
  private rawConfig: Record<string, unknown>;

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
    };
  }

  get enabled(): boolean {
    return this.host !== undefined;
  }

  validate(): void {
    // Host validation
    if (this.host) {
      const hostPattern =
        /^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)*$/i;
      if (!hostPattern.test(this.host)) {
        throw new Error(`Invalid host format: ${this.host}`);
      }
    }

    // Path prefix validation
    if (this.pathPrefix) {
      if (!this.pathPrefix.startsWith("/")) {
        throw new Error(`Path prefix must start with /: ${this.pathPrefix}`);
      }
      if (/[<>"|?*]/.test(this.pathPrefix)) {
        throw new Error(
          `Invalid characters in path prefix: ${this.pathPrefix}`,
        );
      }
    }

    // Health check validation
    const healthcheck = this.healthcheck;
    if (healthcheck?.path) {
      if (!healthcheck.path.startsWith("/")) {
        throw new Error(
          `Health check path must start with /: ${healthcheck.path}`,
        );
      }
      if (/[<>"|?*]/.test(healthcheck.path)) {
        throw new Error(
          `Health check path contains invalid characters: ${healthcheck.path}`,
        );
      }
    }

    if (healthcheck?.interval) {
      if (!/^\d+[smh]$/.test(healthcheck.interval)) {
        throw new Error(
          `Invalid health check interval: ${healthcheck.interval}`,
        );
      }
      // Check for minimum interval
      const match = healthcheck.interval.match(/^(\d+)([smh])$/);
      if (match) {
        const value = parseInt(match[1]);
        const unit = match[2];
        if (unit === "s" && value < 1) {
          throw new Error(
            `Health check interval too short: ${healthcheck.interval}. Minimum is 1s.`,
          );
        }
      }
    }

    // SSL requires host
    if (this.ssl && !this.host) {
      throw new Error("SSL requires a host to be configured");
    }
  }
}
