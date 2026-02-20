/**
 * In-memory DNS cache for service discovery
 *
 * Stores container records indexed by hostname for fast DNS lookups.
 * Handles newest-container-wins logic for zero-downtime deployments.
 */

import type { DnsRecord } from "./types.ts";

/**
 * DNS cache for storing container records
 *
 * Records are indexed by hostname (e.g., "casa-api") and container ID.
 * When multiple containers exist for the same service/server combination,
 * only the newest one is returned in DNS queries.
 */
export class DnsCache {
  /** Records indexed by hostname */
  private byHostname: Map<string, Map<string, DnsRecord>> = new Map();

  /** Records indexed by container ID for fast deletion */
  private byContainerId: Map<string, DnsRecord> = new Map();

  /**
   * Add or update a DNS record
   *
   * @param record DNS record to store
   */
  set(record: DnsRecord): void {
    // Generate hostname(s) for this record
    const hostnames = this.generateHostnames(record);

    // Store by container ID first
    this.byContainerId.set(record.containerId, record);

    // Store by each hostname
    for (const hostname of hostnames) {
      let hostnameRecords = this.byHostname.get(hostname);
      if (!hostnameRecords) {
        hostnameRecords = new Map();
        this.byHostname.set(hostname, hostnameRecords);
      }
      hostnameRecords.set(record.containerId, record);
    }
  }

  /**
   * Get IP addresses for a hostname
   *
   * Returns only healthy containers, preferring the newest one
   * per service/server combination.
   *
   * @param hostname Hostname to lookup (e.g., "casa-api")
   * @returns Array of IP addresses
   */
  get(hostname: string): string[] {
    const normalizedHostname = hostname.toLowerCase();
    const records = this.byHostname.get(normalizedHostname);

    if (!records || records.size === 0) {
      return [];
    }

    // Group records by service+server to handle newest-wins logic
    const byServiceServer = new Map<string, DnsRecord>();

    for (const record of records.values()) {
      if (!record.healthy) {
        continue;
      }

      const key = `${record.service}:${record.serverId}`;
      const existing = byServiceServer.get(key);

      // Keep the newest record per service/server
      if (!existing || record.startedAt > existing.startedAt) {
        byServiceServer.set(key, record);
      }
    }

    // Return all IPs from the selected records
    return Array.from(byServiceServer.values()).map((r) => r.ip);
  }

  /**
   * Get a record by container ID
   *
   * @param containerId Container ID
   * @returns DNS record or undefined
   */
  getByContainerId(containerId: string): DnsRecord | undefined {
    return this.byContainerId.get(containerId);
  }

  /**
   * Remove a record by container ID
   *
   * @param containerId Container ID to remove
   * @returns True if record was found and removed
   */
  remove(containerId: string): boolean {
    const record = this.byContainerId.get(containerId);
    if (!record) {
      return false;
    }

    // Remove from container ID index
    this.byContainerId.delete(containerId);

    // Remove from hostname indices
    const hostnames = this.generateHostnames(record);
    for (const hostname of hostnames) {
      const hostnameRecords = this.byHostname.get(hostname);
      if (hostnameRecords) {
        hostnameRecords.delete(containerId);
        // Clean up empty hostname entry
        if (hostnameRecords.size === 0) {
          this.byHostname.delete(hostname);
        }
      }
    }

    return true;
  }

  /**
   * Update the health status of a container
   *
   * @param containerId Container ID
   * @param healthy New health status
   * @returns True if record was found and updated
   */
  updateHealth(containerId: string, healthy: boolean): boolean {
    const record = this.byContainerId.get(containerId);
    if (!record) {
      return false;
    }

    record.healthy = healthy;
    return true;
  }

  /**
   * Clear all records from the cache
   */
  clear(): void {
    this.byHostname.clear();
    this.byContainerId.clear();
  }

  /**
   * Get total number of records
   */
  get size(): number {
    return this.byContainerId.size;
  }

  /**
   * Get all hostnames in the cache
   */
  get hostnames(): string[] {
    return Array.from(this.byHostname.keys());
  }

  /**
   * Get statistics about the cache
   */
  getStats(): { totalRecords: number; healthyRecords: number; hostnames: number } {
    let healthyRecords = 0;
    for (const r of this.byContainerId.values()) {
      if (r.healthy) healthyRecords++;
    }

    return {
      totalRecords: this.byContainerId.size,
      healthyRecords,
      hostnames: this.byHostname.size,
    };
  }

  /**
   * Generate hostname(s) for a record
   *
   * Generates:
   * - {project}-{service} (primary)
   * - {project}-{service}-{instanceId} (if instanceId is set)
   *
   * @param record DNS record
   * @returns Array of hostnames
   */
  private generateHostnames(record: DnsRecord): string[] {
    const primary = `${record.project}-${record.service}`.toLowerCase();
    const hostnames = [primary];

    if (record.instanceId) {
      const instance = `${primary}-${record.instanceId}`.toLowerCase();
      hostnames.push(instance);
    }

    return hostnames;
  }
}
