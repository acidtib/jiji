import { BaseConfiguration, ConfigurationError } from "./base.ts";
import { log } from "../../utils/logger.ts";

/**
 * Interface for proxy healthcheck configuration
 */
export interface ProxyHealthcheckConfig {
  /** HTTP path to check for health (e.g., "/health", "/up") - mutually exclusive with cmd */
  path?: string;
  /** Command to execute for health check (e.g., "test -f /app/ready") - mutually exclusive with path */
  cmd?: string;
  /** Runtime for command execution (e.g., "docker", "podman") - only used with cmd */
  cmd_runtime?: "docker" | "podman";
  /** Interval between health checks (e.g., "10s", "30s") */
  interval?: string;
  /** Timeout for each health check (e.g., "5s", "10s") */
  timeout?: string;
  /** Maximum time to wait for deployment to become healthy (e.g., "30s", "60s") */
  deploy_timeout?: string;
}

/**
 * Individual proxy target configuration for multi-port services
 */
export interface ProxyTarget {
  /** Application port to proxy (must exist in service's ports array) */
  app_port: number;
  /** Host domain for routing (mutually exclusive with hosts) */
  host?: string;
  /** Multiple host domains for routing (mutually exclusive with host) */
  hosts?: string[];
  /** Enable TLS/SSL for this target */
  ssl?: boolean;
  /** Path prefix for path-based routing */
  path_prefix?: string;
  /** Health check configuration for this target */
  healthcheck?: ProxyHealthcheckConfig;
}

/**
 * Modern ProxyConfiguration for kamal-proxy
 * Supports both single-target and multi-target configurations
 */
export class ProxyConfiguration extends BaseConfiguration {
  private rawConfig: Record<string, unknown>;
  private _targets?: ProxyTarget[];

  // Validation patterns
  // Supports standard hostnames and wildcard domains (e.g., *.example.com)
  private static readonly HOST_PATTERN =
    /^(\*\.)?[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)*$/i;
  private static readonly INVALID_PATH_CHARS = /[<>"|?*]/;
  private static readonly INTERVAL_PATTERN = /^\d+[smh]$/;

  constructor(config: Record<string, unknown>) {
    super(config);
    this.rawConfig = config;
  }

  /**
   * Check if using multi-target syntax
   */
  get isMultiTarget(): boolean {
    return this.has("targets");
  }

  /**
   * Get all proxy targets (normalized from either single or multi-target syntax)
   */
  get targets(): ProxyTarget[] {
    if (!this._targets) {
      if (this.isMultiTarget) {
        this._targets = this.parseTargets();
      } else {
        this._targets = [this.parseSingleTarget()];
      }
    }
    return this._targets;
  }

  /**
   * Check if proxy is enabled (has at least one target with a host)
   */
  get enabled(): boolean {
    return this.targets.length > 0 &&
      this.targets.some((t) =>
        t.host !== undefined || (t.hosts && t.hosts.length > 0)
      );
  }

  /**
   * Parse targets array from configuration
   */
  private parseTargets(): ProxyTarget[] {
    const targetsArray = this.get<unknown[]>("targets");
    if (!Array.isArray(targetsArray)) {
      return [];
    }

    return targetsArray.map((targetConfig: unknown) => {
      const target = targetConfig as Record<string, unknown>;

      return {
        app_port: target.app_port as number,
        host: target.host as string | undefined,
        hosts: Array.isArray(target.hosts)
          ? target.hosts.filter((h): h is string => typeof h === "string")
          : undefined,
        ssl: typeof target.ssl === "boolean" ? target.ssl : false,
        path_prefix: target.path_prefix as string | undefined,
        healthcheck: target.healthcheck
          ? target.healthcheck as ProxyHealthcheckConfig
          : undefined,
      };
    });
  }

  /**
   * Parse single target from root-level configuration
   */
  private parseSingleTarget(): ProxyTarget {
    const hostsValue = this.rawConfig.hosts;
    const hostValue = this.rawConfig.host;

    let hosts: string[] | undefined;
    let host: string | undefined;

    if (Array.isArray(hostsValue)) {
      hosts = hostsValue.filter((h): h is string => typeof h === "string");
    }
    if (typeof hostValue === "string") {
      host = hostValue;
    }

    const healthcheckConfig = this.rawConfig.healthcheck;
    let healthcheck: ProxyHealthcheckConfig | undefined;

    if (healthcheckConfig && typeof healthcheckConfig === "object") {
      const config = healthcheckConfig as Record<string, unknown>;
      healthcheck = {
        path: typeof config.path === "string" ? config.path : undefined,
        cmd: typeof config.cmd === "string" ? config.cmd : undefined,
        cmd_runtime: typeof config.cmd_runtime === "string" &&
            (config.cmd_runtime === "docker" || config.cmd_runtime === "podman")
          ? config.cmd_runtime
          : undefined,
        interval: typeof config.interval === "string"
          ? config.interval
          : undefined,
        timeout: typeof config.timeout === "string"
          ? config.timeout
          : undefined,
        deploy_timeout: typeof config.deploy_timeout === "string"
          ? config.deploy_timeout
          : undefined,
      };
    }

    return {
      app_port: typeof this.rawConfig.app_port === "number"
        ? this.rawConfig.app_port
        : 0,
      host,
      hosts,
      ssl: typeof this.rawConfig.ssl === "boolean" ? this.rawConfig.ssl : false,
      path_prefix: typeof this.rawConfig.path_prefix === "string"
        ? this.rawConfig.path_prefix
        : undefined,
      healthcheck,
    };
  }

  /**
   * Validate a single proxy target
   */
  private validateTarget(
    target: ProxyTarget,
    index: number | null,
  ): ConfigurationError[] {
    const errors: ConfigurationError[] = [];
    const warnings: string[] = [];
    const targetLabel = index !== null ? `target at index ${index}` : "proxy";

    // Validate app_port
    if (!target.app_port || typeof target.app_port !== "number") {
      errors.push(
        new ConfigurationError(
          `${targetLabel} must specify an 'app_port' number`,
        ),
      );
    }

    // Validate host/hosts
    const hasHost = target.host !== undefined;
    const hasHosts = target.hosts !== undefined &&
      target.hosts.length > 0;

    if (!hasHost && !hasHosts) {
      errors.push(
        new ConfigurationError(
          `${targetLabel} must specify either 'host' or 'hosts'`,
        ),
      );
    }

    if (hasHost && hasHosts) {
      errors.push(
        new ConfigurationError(
          `${targetLabel}: use either 'host' or 'hosts', not both`,
        ),
      );
    }

    // Validate host format(s)
    const hosts = hasHosts ? target.hosts! : hasHost ? [target.host!] : [];
    for (const host of hosts) {
      if (!ProxyConfiguration.HOST_PATTERN.test(host)) {
        errors.push(
          new ConfigurationError(
            `Invalid host format in ${targetLabel}: ${host}`,
          ),
        );
      }

      if (host === "localhost" || host === "127.0.0.1") {
        warnings.push(
          `Host '${host}' in ${targetLabel} uses localhost - this may not work in distributed deployments`,
        );
      }
    }

    // Validate path prefix
    if (target.path_prefix) {
      if (!target.path_prefix.startsWith("/")) {
        errors.push(
          new ConfigurationError(
            `Path prefix in ${targetLabel} must start with '/': ${target.path_prefix}`,
          ),
        );
      }

      if (ProxyConfiguration.INVALID_PATH_CHARS.test(target.path_prefix)) {
        errors.push(
          new ConfigurationError(
            `Path prefix in ${targetLabel} contains invalid characters: ${target.path_prefix}`,
          ),
        );
      }

      if (
        target.path_prefix !== "/" && target.path_prefix.endsWith("/")
      ) {
        warnings.push(
          `Path prefix '${target.path_prefix}' in ${targetLabel} has trailing slash - this may affect routing behavior`,
        );
      }
    }

    // Validate healthcheck
    if (target.healthcheck) {
      const hc = target.healthcheck;

      // Check mutual exclusivity between HTTP and command health checks
      if (hc.path && hc.cmd) {
        errors.push(
          new ConfigurationError(
            `Health check in ${targetLabel} cannot specify both 'path' (HTTP) and 'cmd' (command) - use only one`,
          ),
        );
      }

      // Validate HTTP health check
      if (hc.path) {
        if (!hc.path.startsWith("/")) {
          errors.push(
            new ConfigurationError(
              `Health check path in ${targetLabel} must start with /: ${hc.path}`,
            ),
          );
        }
        if (ProxyConfiguration.INVALID_PATH_CHARS.test(hc.path)) {
          errors.push(
            new ConfigurationError(
              `Health check path in ${targetLabel} contains invalid characters: ${hc.path}`,
            ),
          );
        }
        // Warn if cmd_runtime is specified with HTTP health check
        if (hc.cmd_runtime) {
          warnings.push(
            `Health check in ${targetLabel} has 'cmd_runtime' but uses HTTP check (path). cmd_runtime is only used with command-based checks.`,
          );
        }
      }

      // Validate command health check
      if (hc.cmd !== undefined) {
        if (typeof hc.cmd === "string" && hc.cmd.trim().length === 0) {
          errors.push(
            new ConfigurationError(
              `Health check command in ${targetLabel} cannot be empty`,
            ),
          );
        }
        // Note: cmd_runtime is optional and will auto-detect from builder.engine
        // No warning needed since the default behavior is sensible
      }

      // Validate cmd_runtime only makes sense with cmd
      if (hc.cmd_runtime && !hc.cmd) {
        warnings.push(
          `Health check in ${targetLabel} specifies 'cmd_runtime' but has no 'cmd'. cmd_runtime is ignored without a command.`,
        );
      }

      if (hc.interval) {
        if (!ProxyConfiguration.INTERVAL_PATTERN.test(hc.interval)) {
          errors.push(
            new ConfigurationError(
              `Invalid health check interval in ${targetLabel}: ${hc.interval}`,
            ),
          );
        }
        // Check for minimum interval
        const match = hc.interval.match(/^(\d+)([smh])$/);
        if (match) {
          const value = parseInt(match[1]);
          const unit = match[2];
          if (unit === "s" && value < 1) {
            errors.push(
              new ConfigurationError(
                `Health check interval too short in ${targetLabel}: ${hc.interval}. Minimum is 1s.`,
              ),
            );
          }
        }
      }

      if (hc.timeout) {
        if (!ProxyConfiguration.INTERVAL_PATTERN.test(hc.timeout)) {
          errors.push(
            new ConfigurationError(
              `Invalid health check timeout in ${targetLabel}: ${hc.timeout}`,
            ),
          );
        }
      }

      if (hc.deploy_timeout) {
        if (!ProxyConfiguration.INTERVAL_PATTERN.test(hc.deploy_timeout)) {
          errors.push(
            new ConfigurationError(
              `Invalid deploy timeout in ${targetLabel}: ${hc.deploy_timeout}`,
            ),
          );
        }
      }
    }

    // Log warnings
    warnings.forEach((w) => log.warn(w, "proxy"));

    return errors;
  }

  validate(): void {
    const targets = this.targets;

    // Validate empty targets array
    if (this.isMultiTarget && targets.length === 0) {
      throw new ConfigurationError(
        "'targets' array cannot be empty. Define at least one target or remove proxy configuration.",
      );
    }

    // Validate each target
    const allErrors: ConfigurationError[] = [];

    if (this.isMultiTarget) {
      targets.forEach((target, index) => {
        const errors = this.validateTarget(target, index);
        allErrors.push(...errors);
      });
    } else {
      const errors = this.validateTarget(targets[0], null);
      allErrors.push(...errors);
    }

    // Throw first error if any exist
    if (allErrors.length > 0) {
      throw allErrors[0];
    }
  }
}
