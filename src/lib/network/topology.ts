/**
 * Network topology manager
 *
 * Manages network topology through Corrosion distributed database.
 * No longer uses local .jiji/network.json file.
 */

import type { SSHManager } from "../../utils/ssh.ts";
import type { NetworkServer, NetworkTopology } from "../../types/network.ts";
import { log } from "../../utils/logger.ts";
import { getClusterMetadata, queryAllServers } from "./corrosion.ts";
import type { ServerRegistration } from "../../types/network.ts";

/**
 * Load network topology from Corrosion distributed database
 *
 * Queries any server in the cluster via SSH to retrieve the full topology
 *
 * @param ssh - SSH connection to any server in the cluster
 * @returns Network topology or null if cluster not initialized
 */
export async function loadTopology(
  ssh: SSHManager,
): Promise<NetworkTopology | null> {
  try {
    // Query cluster metadata
    const clusterCidr = await getClusterMetadata(ssh, "cluster_cidr");
    const serviceDomain = await getClusterMetadata(ssh, "service_domain");
    const discoveryStr = await getClusterMetadata(ssh, "discovery");
    const createdAt = await getClusterMetadata(ssh, "created_at");

    // If no metadata exists, cluster is not initialized
    if (!clusterCidr || !serviceDomain || !discoveryStr) {
      return null;
    }

    const discovery = discoveryStr as "static" | "corrosion";

    // Query all servers
    const serverRegs = await queryAllServers(ssh);

    // Convert ServerRegistration[] to NetworkServer[]
    const servers: NetworkServer[] = serverRegs.map((reg) => ({
      id: reg.id,
      hostname: reg.hostname,
      subnet: reg.subnet,
      wireguardIp: reg.wireguardIp,
      wireguardPublicKey: reg.wireguardPublicKey,
      managementIp: reg.managementIp,
      endpoints: JSON.parse(reg.endpoints) as string[],
    }));

    const topology: NetworkTopology = {
      clusterCidr,
      serviceDomain,
      discovery,
      servers,
      createdAt: createdAt || new Date().toISOString(),
    };

    log.debug("Loaded network topology from Corrosion", "network");
    return topology;
  } catch (error) {
    log.warn(`Failed to load topology from Corrosion: ${error}`, "network");
    return null;
  }
}

/**
 * Create a new network topology
 *
 * @param clusterCidr - Cluster CIDR (e.g., "10.210.0.0/16")
 * @param serviceDomain - Service domain (e.g., "jiji")
 * @param discovery - Discovery method
 * @returns Empty network topology
 */
export function createTopology(
  clusterCidr: string,
  serviceDomain: string,
  discovery: "static" | "corrosion" = "corrosion",
): NetworkTopology {
  return {
    clusterCidr,
    serviceDomain,
    discovery,
    servers: [],
    createdAt: new Date().toISOString(),
  };
}

/**
 * Add a server to the network topology
 *
 * This is now a helper function that modifies the in-memory topology object.
 * The actual persistence happens via registerServer() in corrosion.ts
 *
 * @param topology - Network topology
 * @param server - Server to add
 * @returns Updated topology
 */
export function addServer(
  topology: NetworkTopology,
  server: NetworkServer,
): NetworkTopology {
  // Check if server already exists
  const existingIndex = topology.servers.findIndex((s) => s.id === server.id);

  if (existingIndex >= 0) {
    // Update existing server
    topology.servers[existingIndex] = server;
    log.debug(`Updated server ${server.id} in topology`, "network");
  } else {
    // Add new server
    topology.servers.push(server);
    log.debug(`Added server ${server.id} to topology`, "network");
  }

  return topology;
}

/**
 * Remove a server from the network topology
 *
 * This is now a helper function that modifies the in-memory topology object.
 *
 * @param topology - Network topology
 * @param serverId - Server ID to remove
 * @returns Updated topology
 */
export function removeServer(
  topology: NetworkTopology,
  serverId: string,
): NetworkTopology {
  topology.servers = topology.servers.filter((s) => s.id !== serverId);
  log.debug(`Removed server ${serverId} from topology`, "network");
  return topology;
}

/**
 * Get a server from the topology by ID
 *
 * @param topology - Network topology
 * @param serverId - Server ID
 * @returns Server or undefined if not found
 */
export function getServer(
  topology: NetworkTopology,
  serverId: string,
): NetworkServer | undefined {
  return topology.servers.find((s) => s.id === serverId);
}

/**
 * Get a server from the topology by hostname
 *
 * @param topology - Network topology
 * @param hostname - Server hostname
 * @returns Server or undefined if not found
 */
export function getServerByHostname(
  topology: NetworkTopology,
  hostname: string,
): NetworkServer | undefined {
  return topology.servers.find((s) => s.hostname === hostname);
}

/**
 * Get all server hostnames from the topology
 *
 * @param topology - Network topology
 * @returns Array of hostnames
 */
export function getAllHostnames(topology: NetworkTopology): string[] {
  return topology.servers.map((s) => s.hostname);
}

/**
 * Generate a unique server ID from hostname
 *
 * @param hostname - Server hostname
 * @param topology - Existing topology (to ensure uniqueness)
 * @returns Unique server ID
 */
export function generateServerId(
  hostname: string,
  topology?: NetworkTopology,
): string {
  // Use hostname as base ID, sanitized
  const baseId = hostname
    .replace(/[^a-zA-Z0-9-]/g, "-")
    .replace(/-+/g, "-")
    .replace(/^-|-$/g, "");

  if (!topology) {
    return baseId;
  }

  // Check for conflicts
  let id = baseId;
  let counter = 1;

  while (topology.servers.some((s) => s.id === id)) {
    id = `${baseId}-${counter}`;
    counter++;
  }

  return id;
}

/**
 * Validate network topology
 *
 * @param topology - Network topology to validate
 * @throws Error if topology is invalid
 */
export function validateTopology(topology: NetworkTopology): void {
  // Check CIDR format
  const cidrRegex = /^(\d{1,3}\.){3}\d{1,3}\/\d{1,2}$/;
  if (!cidrRegex.test(topology.clusterCidr)) {
    throw new Error(`Invalid cluster CIDR: ${topology.clusterCidr}`);
  }

  // Check service domain
  if (!/^[a-z0-9-]+$/.test(topology.serviceDomain)) {
    throw new Error(`Invalid service domain: ${topology.serviceDomain}`);
  }

  // Check discovery method
  if (topology.discovery !== "static" && topology.discovery !== "corrosion") {
    throw new Error(`Invalid discovery method: ${topology.discovery}`);
  }

  // Validate each server
  const serverIds = new Set<string>();
  const hostnames = new Set<string>();
  const subnets = new Set<string>();

  for (const server of topology.servers) {
    // Check for duplicate IDs
    if (serverIds.has(server.id)) {
      throw new Error(`Duplicate server ID: ${server.id}`);
    }
    serverIds.add(server.id);

    // Check for duplicate hostnames
    if (hostnames.has(server.hostname)) {
      throw new Error(`Duplicate server hostname: ${server.hostname}`);
    }
    hostnames.add(server.hostname);

    // Check for duplicate subnets
    if (subnets.has(server.subnet)) {
      throw new Error(`Duplicate subnet: ${server.subnet}`);
    }
    subnets.add(server.subnet);

    // Validate subnet format
    if (!cidrRegex.test(server.subnet)) {
      throw new Error(
        `Invalid subnet for server ${server.id}: ${server.subnet}`,
      );
    }

    // Validate WireGuard IP format
    const ipRegex = /^(\d{1,3}\.){3}\d{1,3}$/;
    if (!ipRegex.test(server.wireguardIp)) {
      throw new Error(
        `Invalid WireGuard IP for server ${server.id}: ${server.wireguardIp}`,
      );
    }

    // Validate management IP (IPv6)
    const ipv6Regex = /^[0-9a-fA-F:]+$/;
    if (!ipv6Regex.test(server.managementIp)) {
      throw new Error(
        `Invalid management IP for server ${server.id}: ${server.managementIp}`,
      );
    }

    // Validate WireGuard public key (base64)
    if (!server.wireguardPublicKey || server.wireguardPublicKey.length !== 44) {
      throw new Error(
        `Invalid WireGuard public key for server ${server.id}`,
      );
    }

    // Validate endpoints array
    if (!Array.isArray(server.endpoints) || server.endpoints.length === 0) {
      throw new Error(`Server ${server.id} must have at least one endpoint`);
    }
  }

  log.debug("Network topology validation passed", "network");
}

/**
 * Get network statistics from topology
 *
 * @param topology - Network topology
 * @returns Statistics object
 */
export function getTopologyStats(topology: NetworkTopology): {
  serverCount: number;
  oldestServer: string;
  newestServer: string;
  clusterAge: string;
} {
  if (topology.servers.length === 0) {
    return {
      serverCount: 0,
      oldestServer: "N/A",
      newestServer: "N/A",
      clusterAge: "N/A",
    };
  }

  const createdDate = new Date(topology.createdAt);
  const now = new Date();
  const ageMs = now.getTime() - createdDate.getTime();
  const ageDays = Math.floor(ageMs / (1000 * 60 * 60 * 24));

  let clusterAge: string;
  if (ageDays === 0) {
    clusterAge = "Today";
  } else if (ageDays === 1) {
    clusterAge = "1 day";
  } else {
    clusterAge = `${ageDays} days`;
  }

  return {
    serverCount: topology.servers.length,
    oldestServer: topology.servers[0]?.hostname || "N/A",
    newestServer: topology.servers[topology.servers.length - 1]?.hostname ||
      "N/A",
    clusterAge,
  };
}

/**
 * Check if network topology exists in Corrosion
 *
 * @param ssh - SSH connection to any server
 * @returns True if topology exists in Corrosion
 */
export async function topologyExists(ssh: SSHManager): Promise<boolean> {
  try {
    const clusterCidr = await getClusterMetadata(ssh, "cluster_cidr");
    return clusterCidr !== null;
  } catch {
    return false;
  }
}

/**
 * Get the next available server index for subnet allocation
 *
 * @param topology - Network topology
 * @returns Next server index
 */
export function getNextServerIndex(topology: NetworkTopology): number {
  return topology.servers.length;
}
