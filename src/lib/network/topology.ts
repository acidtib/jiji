/**
 * Network topology manager
 *
 * Manages the .jiji/network.json file that stores network state
 * including server information, WireGuard keys, and IP allocations.
 */

import { ensureDir, exists } from "@std/fs";
import { join } from "@std/path";
import type { NetworkServer, NetworkTopology } from "../../types/network.ts";
import { log } from "../../utils/logger.ts";

const NETWORK_CONFIG_FILE = ".jiji/network.json";

/**
 * Load network topology from .jiji/network.json
 *
 * @param configDir - Directory containing the config (defaults to cwd)
 * @returns Network topology or null if not exists
 */
export async function loadTopology(
  configDir?: string,
): Promise<NetworkTopology | null> {
  const configPath = configDir
    ? join(configDir, NETWORK_CONFIG_FILE)
    : NETWORK_CONFIG_FILE;

  if (!await exists(configPath)) {
    return null;
  }

  try {
    const content = await Deno.readTextFile(configPath);
    const topology = JSON.parse(content) as NetworkTopology;

    log.debug(`Loaded network topology from ${configPath}`, "network");
    return topology;
  } catch (error) {
    throw new Error(
      `Failed to load network topology from ${configPath}: ${error}`,
    );
  }
}

/**
 * Save network topology to .jiji/network.json
 *
 * @param topology - Network topology to save
 * @param configDir - Directory to save config (defaults to cwd)
 */
export async function saveTopology(
  topology: NetworkTopology,
  configDir?: string,
): Promise<void> {
  const configPath = configDir
    ? join(configDir, NETWORK_CONFIG_FILE)
    : NETWORK_CONFIG_FILE;

  // Ensure .jiji directory exists
  const dir = configDir ? join(configDir, ".jiji") : ".jiji";
  await ensureDir(dir);

  // Update timestamp
  topology.updatedAt = new Date().toISOString();

  try {
    const content = JSON.stringify(topology, null, 2);
    await Deno.writeTextFile(configPath, content);

    log.debug(`Saved network topology to ${configPath}`, "network");
  } catch (error) {
    throw new Error(
      `Failed to save network topology to ${configPath}: ${error}`,
    );
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
 * Check if network topology exists
 *
 * @param configDir - Directory to check (defaults to cwd)
 * @returns True if topology file exists
 */
export async function topologyExists(configDir?: string): Promise<boolean> {
  const configPath = configDir
    ? join(configDir, NETWORK_CONFIG_FILE)
    : NETWORK_CONFIG_FILE;

  return await exists(configPath);
}

/**
 * Delete network topology file
 *
 * @param configDir - Directory containing config (defaults to cwd)
 */
export async function deleteTopology(configDir?: string): Promise<void> {
  const configPath = configDir
    ? join(configDir, NETWORK_CONFIG_FILE)
    : NETWORK_CONFIG_FILE;

  if (await exists(configPath)) {
    await Deno.remove(configPath);
    log.debug(`Deleted network topology from ${configPath}`, "network");
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
