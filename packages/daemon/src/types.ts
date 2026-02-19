/**
 * Configuration for the daemon, parsed from environment variables.
 * All env vars use the JIJI_ prefix to avoid collisions.
 */
export interface Config {
  serverId: string;
  engine: "docker" | "podman";
  interfaceName: string;
  corrosionApi: string;
  corrosionDir: string;
  loopInterval: number;
}

/**
 * Parse configuration from environment variables.
 */
export function parseConfig(): Config {
  const serverId = Deno.env.get("JIJI_SERVER_ID");
  if (!serverId) {
    throw new Error("JIJI_SERVER_ID environment variable is required");
  }

  const engine = Deno.env.get("JIJI_ENGINE") ?? "docker";
  if (engine !== "docker" && engine !== "podman") {
    throw new Error(
      `Invalid JIJI_ENGINE: ${engine}. Must be 'docker' or 'podman'`,
    );
  }

  const interfaceName = Deno.env.get("JIJI_INTERFACE") ?? "jiji0";
  const corrosionApi = Deno.env.get("JIJI_CORROSION_API") ??
    "http://127.0.0.1:31220";
  const corrosionDir = Deno.env.get("JIJI_CORROSION_DIR") ??
    "/opt/jiji/corrosion";
  const loopInterval = parseInt(
    Deno.env.get("JIJI_LOOP_INTERVAL") ?? "30",
    10,
  );

  if (isNaN(loopInterval) || loopInterval < 1) {
    throw new Error("JIJI_LOOP_INTERVAL must be a positive integer");
  }

  return {
    serverId,
    engine,
    interfaceName,
    corrosionApi,
    corrosionDir,
    loopInterval,
  };
}

/**
 * Corrosion HTTP API transaction result.
 */
export interface TransactionResult {
  results: TransactionRowResult[];
}

export interface TransactionRowResult {
  rows_affected: number;
  columns?: string[];
  rows?: unknown[][];
}

/**
 * WireGuard peer state from `wg show dump`.
 */
export interface PeerState {
  publicKey: string;
  presharedKey: string;
  endpoint: string;
  allowedIps: string;
  latestHandshake: number;
  transferRx: number;
  transferTx: number;
  persistentKeepalive: string;
}

/**
 * Server record from Corrosion.
 */
export interface ServerRecord {
  id: string;
  wireguardPubkey: string;
  subnet: string;
  managementIp: string;
  endpoints: string[];
  lastSeen: number;
  hostname?: string;
}

/**
 * Container record from Corrosion.
 */
export interface ContainerRecord {
  id: string;
  ip: string;
  healthPort: number | null;
  healthStatus: string;
  consecutiveFailures: number;
  serverId: string;
  service?: string;
  startedAt?: number;
}

/**
 * Result of a container health check.
 */
export interface HealthResult {
  containerId: string;
  newStatus: string;
  newFailures: number;
  changed: boolean;
}

/**
 * Constants used across the daemon.
 */
export const PEER_DOWN_THRESHOLD = 275; // seconds - from WireGuard spec
export const STALE_CONTAINER_THRESHOLD = 180; // 3 minutes in seconds
export const OFFLINE_SERVER_THRESHOLD = 600000; // 10 minutes in milliseconds
export const HEARTBEAT_STALE_THRESHOLD = 120000; // 2 minutes in milliseconds
export const ACTIVE_SERVER_THRESHOLD = 300000; // 5 minutes in milliseconds
