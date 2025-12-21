import { BaseConfiguration, ConfigurationError } from "./base.ts";
import type { NetworkDiscovery } from "../../types/network.ts";

/**
 * Network configuration for private networking
 */
export class NetworkConfiguration extends BaseConfiguration {
  private _enabled?: boolean;
  private _clusterCidr?: string;
  private _discovery?: NetworkDiscovery;

  /**
   * Whether private networking is enabled
   * Defaults to true
   */
  get enabled(): boolean {
    if (this._enabled === undefined) {
      this._enabled = this.get<boolean>("enabled", true);
      if (typeof this._enabled !== "boolean") {
        throw new ConfigurationError(
          `network.enabled must be a boolean`,
        );
      }
    }
    return this._enabled;
  }

  /**
   * CIDR range for the cluster network
   * Defaults to 10.210.0.0/16
   */
  get clusterCidr(): string {
    if (!this._clusterCidr) {
      this._clusterCidr = this.get<string>(
        "cluster_cidr",
        "10.210.0.0/16",
      );
      this.validateCIDR(this._clusterCidr, "network.cluster_cidr");
    }
    return this._clusterCidr;
  }

  /**
   * Service domain for DNS resolution
   * Always returns 'jiji'
   */
  get serviceDomain(): string {
    return "jiji";
  }

  /**
   * Network discovery method
   * Defaults to 'corrosion'
   */
  get discovery(): NetworkDiscovery {
    if (!this._discovery) {
      const discovery = this.get<string>("discovery", "corrosion");
      if (discovery !== "static" && discovery !== "corrosion") {
        throw new ConfigurationError(
          `Invalid network.discovery value: ${discovery}. Must be 'static' or 'corrosion'`,
        );
      }
      this._discovery = discovery as NetworkDiscovery;
    }
    return this._discovery;
  }

  /**
   * Validates CIDR notation
   */
  private validateCIDR(cidr: string, path: string): void {
    const cidrRegex = /^(\d{1,3}\.){3}\d{1,3}\/\d{1,2}$/;
    if (!cidrRegex.test(cidr)) {
      throw new ConfigurationError(
        `Invalid CIDR notation at ${path}: ${cidr}`,
      );
    }

    const [ip, prefix] = cidr.split("/");
    const prefixNum = parseInt(prefix, 10);

    if (prefixNum < 0 || prefixNum > 32) {
      throw new ConfigurationError(
        `Invalid CIDR prefix at ${path}: /${prefix}. Must be between 0 and 32`,
      );
    }

    const octets = ip.split(".").map((octet) => parseInt(octet, 10));
    for (const octet of octets) {
      if (octet < 0 || octet > 255) {
        throw new ConfigurationError(
          `Invalid IP address in CIDR at ${path}: ${ip}`,
        );
      }
    }
  }

  /**
   * Validates the network configuration
   */
  validate(): void {
    this.enabled;
    this.clusterCidr;
    this.serviceDomain;
    this.discovery;

    if (!/^[a-z0-9-]+$/.test(this.serviceDomain)) {
      throw new ConfigurationError(
        `Invalid service domain: ${this.serviceDomain}. Must contain only lowercase letters, numbers, and hyphens`,
      );
    }

    const [, prefix] = this.clusterCidr.split("/");
    const prefixNum = parseInt(prefix, 10);

    if (this.enabled && prefixNum > 24) {
      throw new ConfigurationError(
        `Cluster CIDR prefix /${prefix} is too small. Use at least /24 to support multiple servers`,
      );
    }
  }

  /**
   * Returns the configuration as a plain object
   */
  toObject(): Record<string, unknown> {
    const result: Record<string, unknown> = {};

    if (this.enabled !== undefined) {
      result.enabled = this.enabled;
    }

    if (this._clusterCidr !== undefined) {
      result.cluster_cidr = this.clusterCidr;
    }

    result.service_domain = this.serviceDomain;

    if (this._discovery !== undefined) {
      result.discovery = this.discovery;
    }

    return result;
  }

  /**
   * Calculates the number of available /24 subnets in the cluster CIDR
   */
  getAvailableSubnetCount(): number {
    const [, prefix] = this.clusterCidr.split("/");
    const prefixNum = parseInt(prefix, 10);

    return Math.pow(2, 24 - prefixNum);
  }

  /**
   * Gets the base IP from the cluster CIDR
   */
  getBaseIp(): string {
    const [ip] = this.clusterCidr.split("/");
    return ip;
  }

  /**
   * Gets the prefix length from the cluster CIDR
   */
  getPrefixLength(): number {
    const [, prefix] = this.clusterCidr.split("/");
    return parseInt(prefix, 10);
  }
}
