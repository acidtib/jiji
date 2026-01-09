/**
 * Network-related type definitions for Jiji private networking
 */

/**
 * Network discovery method
 * Only "corrosion" is supported (distributed CRDT-based service discovery)
 */
export type NetworkDiscovery = "corrosion";

/**
 * Network server information stored in Corrosion distributed database
 */
export interface NetworkServer {
  id: string;
  hostname: string;
  subnet: string;
  wireguardIp: string;
  wireguardPublicKey: string;
  managementIp: string;
  endpoints: string[];
  publicIp?: string; // Discovered public IP for NAT traversal
  privateIps?: string[]; // Discovered private IPs for multi-homed servers
}

/**
 * Network topology stored in Corrosion distributed database
 */
export interface NetworkTopology {
  clusterCidr: string;
  serviceDomain: string;
  discovery: NetworkDiscovery;
  servers: NetworkServer[];
  createdAt: string;
  updatedAt?: string;
}

/**
 * WireGuard peer configuration
 */
export interface WireGuardPeer {
  publicKey: string;
  allowedIps: string[];
  endpoint?: string;
  persistentKeepalive?: number;
}

/**
 * WireGuard interface configuration
 */
export interface WireGuardConfig {
  privateKey: string;
  address: string[];
  listenPort: number;
  mtu?: number; // Maximum Transmission Unit (default: 1420 for WireGuard)
  peers: WireGuardPeer[];
}

/**
 * Corrosion configuration
 */
export interface CorrosionConfig {
  dbPath: string;
  schemaPath: string;
  gossipAddr: string;
  apiAddr: string;
  adminPath: string;
  bootstrap: string[];
  plaintext: boolean;
}

/**
 * DNS server configuration
 */
export interface DNSConfig {
  listenAddr: string;
  serviceDomain: string;
  corrosionApiAddr: string;
  upstreamResolvers?: string[];
}

/**
 * Health status for granular container health tracking
 */
export type ContainerHealthStatus =
  | "healthy"
  | "degraded"
  | "unhealthy"
  | "unknown";

/**
 * Container registration info for Corrosion
 */
export interface ContainerRegistration {
  id: string;
  service: string;
  serverId: string;
  ip: string;
  startedAt: number;
  instanceId?: string; // Identifier for multi-server deployments (e.g., "primary", "157-230-162-210")
  healthStatus?: ContainerHealthStatus; // Defaults to "healthy" when not specified
  lastHealthCheck?: number;
  consecutiveFailures?: number;
  healthPort?: number; // Port to check for TCP health (from proxy app_port)
}

/**
 * Service registration info for Corrosion
 */
export interface ServiceRegistration {
  name: string;
  project: string;
}

/**
 * Server registration info for Corrosion
 */
export interface ServerRegistration {
  id: string;
  hostname: string;
  subnet: string;
  wireguardIp: string;
  wireguardPublicKey: string;
  managementIp: string;
  endpoints: string[];
  lastSeen: number;
}

/**
 * Network setup result for a single server
 */
export interface NetworkSetupResult {
  host: string;
  success: boolean;
  message?: string;
  error?: string;
  publicKey?: string;
}

/**
 * Network installation dependencies
 */
export interface NetworkDependencies {
  wireguard: boolean;
  corrosion: boolean;
  dns: boolean;
}

/**
 * Network status information
 */
export interface NetworkStatus {
  enabled: boolean;
  servers: Array<{
    hostname: string;
    wireguardIp: string;
    subnet: string;
    online: boolean;
    containers: Array<{
      id: string;
      service: string;
      ip: string;
      healthStatus: ContainerHealthStatus;
    }>;
  }>;
}
