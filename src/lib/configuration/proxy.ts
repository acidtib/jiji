import { BaseConfiguration } from "./base.ts";

/**
 * Interface for proxy healthcheck configuration
 */
export interface ProxyHealthcheckConfig {
  path?: string;
  interval?: string;
}

/**
 * ProxyConfiguration handles kamal-proxy settings for a service
 *
 * Manages proxy configuration including:
 * - SSL/TLS termination
 * - Host/domain routing
 * - Health check configuration
 *
 * Key Pattern: Lazy loading - properties are parsed only when accessed via getters
 */
export class ProxyConfiguration extends BaseConfiguration {
  private rawConfig: Record<string, unknown>;

  constructor(config: Record<string, unknown>) {
    super(config);
    this.rawConfig = config;
  }

  /**
   * Whether SSL/TLS is enabled for this service
   * @returns true if SSL should be enabled
   */
  get ssl(): boolean {
    const value = this.get<boolean>("ssl", false);
    return typeof value === "boolean" ? value : false;
  }

  /**
   * The host/domain name for this service
   * @returns host string or undefined if not configured
   */
  get host(): string | undefined {
    const value = this.get<string | undefined>("host");
    return typeof value === "string" ? value : undefined;
  }

  /**
   * Health check configuration
   * @returns healthcheck config object or undefined
   */
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

  /**
   * Whether proxy is enabled (has host configured)
   * @returns true if proxy should be deployed
   */
  get enabled(): boolean {
    return this.host !== undefined;
  }

  /**
   * Validate proxy configuration
   * @throws Error if configuration is invalid
   */
  validate(): void {
    if (this.enabled && !this.host) {
      throw new Error("Proxy host is required when proxy is configured");
    }

    // Validate healthcheck interval format if present
    const healthcheck = this.healthcheck;
    if (healthcheck?.interval) {
      if (!/^\d+[smh]$/.test(healthcheck.interval)) {
        throw new Error(
          `Invalid healthcheck interval format: ${healthcheck.interval}. Must be like "10s", "5m", or "1h"`,
        );
      }
    }

    // Validate healthcheck path if present
    if (healthcheck?.path && !healthcheck.path.startsWith("/")) {
      throw new Error(
        `Healthcheck path must start with /: ${healthcheck.path}`,
      );
    }
  }
}
