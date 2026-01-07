/**
 * Corrosion distributed database service
 *
 * Handles installation, configuration, and interaction with Corrosion
 * for service discovery and state management.
 */

import type { SSHManager } from "../../utils/ssh.ts";
import type { CommandResult } from "../../types/ssh.ts";
import { log } from "../../utils/logger.ts";
import type {
  ContainerRegistration,
  CorrosionConfig,
  ServerRegistration,
  ServiceRegistration,
} from "../../types/network.ts";
import {
  CORROSION_SYNC_LOG_INTERVAL_MS,
  CORROSION_SYNC_POLL_INTERVAL_MS,
  CORROSION_SYNC_TIMEOUT_SECONDS,
} from "../../constants.ts";

const CORROSION_REPO = "superfly/corrosion";
const CORROSION_INSTALL_DIR = "/opt/jiji/corrosion";

/**
 * Escape SQL string values to prevent SQL injection
 * Follows SQLite string literal escaping rules
 */
function escapeSql(value: string): string {
  return value.replace(/'/g, "''");
}

/**
 * Internal helper: Execute a Corrosion SELECT query
 * Handles escaping and formatting consistently
 */
function corrosionQuery(ssh: SSHManager, sql: string): Promise<CommandResult> {
  const escapedSql = sql.replace(/"/g, '\\"').replace(/\n/g, " ");
  return ssh.executeCommand(
    `${CORROSION_INSTALL_DIR}/corrosion query --config ${CORROSION_INSTALL_DIR}/config.toml "${escapedSql}"`,
  );
}

/**
 * Internal helper: Execute a Corrosion write operation (INSERT/UPDATE/DELETE)
 * Handles escaping and formatting consistently
 */
function corrosionExec(ssh: SSHManager, sql: string): Promise<CommandResult> {
  const escapedSql = sql.replace(/"/g, '\\"').replace(/\n/g, " ");
  return ssh.executeCommand(
    `${CORROSION_INSTALL_DIR}/corrosion exec --config ${CORROSION_INSTALL_DIR}/config.toml "${escapedSql}"`,
  );
}

/**
 * Database schema for Corrosion
 */
const CORROSION_SCHEMA = `-- Jiji network database schema

-- Cluster metadata (cluster-wide configuration)
CREATE TABLE IF NOT EXISTS cluster_metadata (
  key TEXT NOT NULL PRIMARY KEY,
  value TEXT NOT NULL DEFAULT ''
);

-- Servers in the cluster
CREATE TABLE IF NOT EXISTS servers (
  id TEXT NOT NULL PRIMARY KEY,
  hostname TEXT NOT NULL DEFAULT '',
  subnet TEXT NOT NULL DEFAULT '',
  wireguard_ip TEXT NOT NULL DEFAULT '',
  wireguard_pubkey TEXT NOT NULL DEFAULT '',
  management_ip TEXT NOT NULL DEFAULT '',
  endpoints TEXT NOT NULL DEFAULT '',
  last_seen INTEGER NOT NULL DEFAULT 0
);

-- Services deployed
CREATE TABLE IF NOT EXISTS services (
  name TEXT NOT NULL PRIMARY KEY,
  project TEXT NOT NULL DEFAULT ''
);

-- Containers running in the cluster
CREATE TABLE IF NOT EXISTS containers (
  id TEXT NOT NULL PRIMARY KEY,
  service TEXT NOT NULL DEFAULT '',
  server_id TEXT NOT NULL DEFAULT '',
  ip TEXT NOT NULL DEFAULT '',
  healthy INTEGER DEFAULT 1,
  started_at INTEGER NOT NULL DEFAULT 0,
  instance_id TEXT DEFAULT NULL,
  -- Phase 3: Granular health tracking
  health_status TEXT DEFAULT 'unknown',
  last_health_check INTEGER DEFAULT 0,
  consecutive_failures INTEGER DEFAULT 0,
  health_port INTEGER DEFAULT NULL
);

-- Enable CRDT for all tables (disabled for compatibility)
-- SELECT crsql_as_crr('cluster_metadata');
-- SELECT crsql_as_crr('servers');
-- SELECT crsql_as_crr('services');
-- SELECT crsql_as_crr('containers');

-- Performance indexes for common queries
CREATE INDEX IF NOT EXISTS idx_containers_server_id ON containers(server_id);
CREATE INDEX IF NOT EXISTS idx_containers_service ON containers(service);
CREATE INDEX IF NOT EXISTS idx_containers_healthy ON containers(healthy);
CREATE INDEX IF NOT EXISTS idx_containers_health_status ON containers(health_status);
CREATE INDEX IF NOT EXISTS idx_servers_last_seen ON servers(last_seen);
`;

/**
 * Apply schema migrations for existing deployments
 * Adds new columns if they don't exist (safe to run multiple times)
 *
 * @param ssh - SSH connection to a server in the cluster
 * @returns True if migration was successful
 */
export async function applyMigrations(ssh: SSHManager): Promise<boolean> {
  const host = ssh.getHost();

  try {
    // Check if containers table has health_status column
    const checkResult = await corrosionQuery(
      ssh,
      "SELECT COUNT(*) FROM pragma_table_info('containers') WHERE name = 'health_status';",
    );

    if (checkResult.code !== 0) {
      log.debug(
        `Migration check failed on ${host}: ${checkResult.stderr}`,
        "corrosion",
      );
      return false;
    }

    const hasHealthStatus = checkResult.stdout.trim() !== "0";

    if (!hasHealthStatus) {
      log.info(`Applying Phase 3 migration on ${host}...`, "corrosion");

      // Add columns one at a time (SQLite requires separate ALTER TABLE statements)
      const columns = [
        "ALTER TABLE containers ADD COLUMN health_status TEXT DEFAULT 'unknown';",
        "ALTER TABLE containers ADD COLUMN last_health_check INTEGER DEFAULT 0;",
        "ALTER TABLE containers ADD COLUMN consecutive_failures INTEGER DEFAULT 0;",
        "ALTER TABLE containers ADD COLUMN health_port INTEGER DEFAULT NULL;",
        "CREATE INDEX IF NOT EXISTS idx_containers_health_status ON containers(health_status);",
      ];

      for (const sql of columns) {
        const result = await corrosionExec(ssh, sql);

        // Ignore "duplicate column" errors
        if (
          result.code !== 0 &&
          !result.stderr.includes("duplicate column") &&
          !result.stderr.includes("already exists")
        ) {
          log.warn(
            `Migration statement failed: ${result.stderr}`,
            "corrosion",
          );
        }
      }

      // Initialize existing containers with 'healthy' status based on current healthy column
      await corrosionExec(
        ssh,
        "UPDATE containers SET health_status = CASE WHEN healthy = 1 THEN 'healthy' ELSE 'unhealthy' END WHERE health_status = 'unknown' OR health_status IS NULL;",
      );

      log.success(`Phase 3 migration complete on ${host}`, "corrosion");
    } else {
      log.debug(`Migration already applied on ${host}`, "corrosion");
    }

    return true;
  } catch (error) {
    log.error(`Migration failed on ${host}: ${error}`, "corrosion");
    return false;
  }
}

/**
 * Install Corrosion on a remote server
 *
 * @param ssh - SSH connection to the server
 * @returns True if installation was successful
 */
export async function installCorrosion(ssh: SSHManager): Promise<boolean> {
  const host = ssh.getHost();

  // Check if Corrosion is already installed
  const checkResult = await ssh.executeCommand(
    `test -f ${CORROSION_INSTALL_DIR}/corrosion && echo "exists"`,
  );

  if (checkResult.stdout.includes("exists")) {
    return true;
  }

  // Create installation directory
  await ssh.executeCommand(`mkdir -p ${CORROSION_INSTALL_DIR}`);

  // Detect architecture
  const archResult = await ssh.executeCommand("uname -m");
  const arch = archResult.stdout.trim();

  let downloadArch: string;
  if (arch === "x86_64" || arch === "amd64") {
    downloadArch = "x86_64";
  } else if (arch === "aarch64" || arch === "arm64") {
    downloadArch = "aarch64";
  } else {
    log.error(`Unsupported architecture: ${arch}`, "corrosion");
    return false;
  }

  try {
    // Download latest release from psviderski/corrosion
    const downloadUrl =
      `https://github.com/${CORROSION_REPO}/releases/latest/download/corrosion-${downloadArch}-unknown-linux-gnu.tar.gz`;

    const downloadResult = await ssh.executeCommand(
      `cd ${CORROSION_INSTALL_DIR} && curl -fsSL "${downloadUrl}" -o corrosion.tar.gz`,
    );

    if (downloadResult.code !== 0) {
      throw new Error(
        `Failed to download Corrosion: ${downloadResult.stderr}`,
      );
    }

    // Extract
    const extractResult = await ssh.executeCommand(
      `cd ${CORROSION_INSTALL_DIR} && tar -xzf corrosion.tar.gz && rm corrosion.tar.gz`,
    );

    if (extractResult.code !== 0) {
      throw new Error(`Failed to extract Corrosion: ${extractResult.stderr}`);
    }

    // Make executable
    await ssh.executeCommand(`chmod +x ${CORROSION_INSTALL_DIR}/corrosion`);

    return true;
  } catch (error) {
    log.error(`Failed to install Corrosion on ${host}: ${error}`, "corrosion");
    return false;
  }
}

/**
 * Generate Corrosion configuration file content
 *
 * @param config - Corrosion configuration
 * @returns TOML configuration content
 */
export function generateCorrosionConfig(config: CorrosionConfig): string {
  const initialPeers = config.bootstrap
    .map((peer) => `"${peer}"`)
    .join(", ");

  return `[db]
path = "${config.dbPath}"
schema_paths = ["${config.schemaPath}"]

[gossip]
addr = "${config.gossipAddr}"
bootstrap = [${initialPeers}]
plaintext = ${config.plaintext}

[api]
addr = "${config.apiAddr}"

[admin]
path = "${config.adminPath}"
`;
}

/**
 * Write Corrosion configuration to a remote server
 *
 * @param ssh - SSH connection to the server
 * @param config - Corrosion configuration
 */
export async function writeCorrosionConfig(
  ssh: SSHManager,
  config: CorrosionConfig,
): Promise<void> {
  const configContent = generateCorrosionConfig(config);
  const configPath = `${CORROSION_INSTALL_DIR}/config.toml`;

  // Ensure directory exists
  await ssh.executeCommand(`mkdir -p ${CORROSION_INSTALL_DIR}/schemas`);

  // Write config
  const writeResult = await ssh.executeCommand(
    `cat > ${configPath} << 'EOFCORR'\n${configContent}\nEOFCORR`,
  );

  if (writeResult.code !== 0) {
    throw new Error(
      `Failed to write Corrosion config: ${writeResult.stderr}`,
    );
  }

  // Write schema
  const schemaPath = `${CORROSION_INSTALL_DIR}/schemas/jiji.sql`;
  const schemaResult = await ssh.executeCommand(
    `cat > ${schemaPath} << 'EOFSQL'\n${CORROSION_SCHEMA}\nEOFSQL`,
  );

  if (schemaResult.code !== 0) {
    throw new Error(`Failed to write Corrosion schema: ${schemaResult.stderr}`);
  }
}

/**
 * Create systemd service for Corrosion
 *
 * @param ssh - SSH connection to the server
 */
export async function createCorrosionService(ssh: SSHManager): Promise<void> {
  const serviceContent = `[Unit]
Description=Corrosion distributed database for Jiji
After=network.target

[Service]
Type=simple
ExecStart=${CORROSION_INSTALL_DIR}/corrosion agent --config ${CORROSION_INSTALL_DIR}/config.toml
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
`;

  const serviceResult = await ssh.executeCommand(
    `cat > /etc/systemd/system/jiji-corrosion.service << 'EOFSVC'\n${serviceContent}\nEOFSVC`,
  );

  if (serviceResult.code !== 0) {
    throw new Error(
      `Failed to create Corrosion service: ${serviceResult.stderr}`,
    );
  }

  // Reload systemd
  await ssh.executeCommand("systemctl daemon-reload");
}

/**
 * Start Corrosion service
 *
 * @param ssh - SSH connection to the server
 */
export async function startCorrosionService(ssh: SSHManager): Promise<void> {
  const result = await ssh.executeCommand(
    "systemctl enable --now jiji-corrosion",
  );

  if (result.code !== 0) {
    throw new Error(`Failed to start Corrosion service: ${result.stderr}`);
  }

  // Wait for Corrosion API to be ready
  const maxRetries = 10;
  const retryDelay = 1000;
  let ready = false;

  for (let i = 0; i < maxRetries; i++) {
    await new Promise((resolve) => setTimeout(resolve, retryDelay));

    // Check if API is responding
    const checkResult = await ssh.executeCommand(
      "curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8080/health || echo '000'",
    );

    const statusCode = checkResult.stdout.trim();
    if (statusCode === "200" || statusCode === "404") {
      // 404 is also acceptable - means server is up but /health endpoint doesn't exist
      ready = true;
      break;
    }
  }

  if (!ready) {
    throw new Error(
      "Corrosion service did not become ready within expected time",
    );
  }
}

/**
 * Stop Corrosion service
 *
 * @param ssh - SSH connection to the server
 */
export async function stopCorrosionService(ssh: SSHManager): Promise<void> {
  await ssh.executeCommand("systemctl stop jiji-corrosion");
}

/**
 * Register a server in Corrosion database
 *
 * @param ssh - SSH connection to the server
 * @param server - Server registration info
 */
export async function registerServer(
  ssh: SSHManager,
  server: ServerRegistration,
): Promise<void> {
  const endpointsJson = JSON.stringify(server.endpoints);

  const sql =
    `INSERT OR REPLACE INTO servers (id, hostname, subnet, wireguard_ip, wireguard_pubkey, management_ip, endpoints, last_seen) VALUES ('${
      escapeSql(server.id)
    }', '${escapeSql(server.hostname)}', '${escapeSql(server.subnet)}', '${
      escapeSql(server.wireguardIp)
    }', '${escapeSql(server.wireguardPublicKey)}', '${
      escapeSql(server.managementIp)
    }', '${endpointsJson}', ${server.lastSeen});`;

  const result = await corrosionExec(ssh, sql);

  if (result.code !== 0) {
    throw new Error(`Failed to register server: ${result.stderr}`);
  }
}

/**
 * Register a service in Corrosion database
 *
 * @param ssh - SSH connection to the server
 * @param service - Service registration info
 */
export async function registerService(
  ssh: SSHManager,
  service: ServiceRegistration,
): Promise<void> {
  const sql = `
    INSERT OR REPLACE INTO services (name, project)
    VALUES ('${escapeSql(service.name)}', '${escapeSql(service.project)}');
  `;

  const result = await corrosionExec(ssh, sql);

  if (result.code !== 0) {
    throw new Error(`Failed to register service: ${result.stderr}`);
  }
}

/**
 * Register a container in Corrosion database
 *
 * @param ssh - SSH connection to the server
 * @param container - Container registration info
 */
export async function registerContainer(
  ssh: SSHManager,
  container: ContainerRegistration,
): Promise<void> {
  const healthy = container.healthy ? 1 : 0;
  const instanceId = container.instanceId !== undefined
    ? `'${escapeSql(container.instanceId)}'`
    : "NULL";

  const sql = `
    INSERT OR REPLACE INTO containers
    (id, service, server_id, ip, healthy, started_at, instance_id)
    VALUES
    ('${escapeSql(container.id)}', '${escapeSql(container.service)}', '${
    escapeSql(container.serverId)
  }',
     '${
    escapeSql(container.ip)
  }', ${healthy}, ${container.startedAt}, ${instanceId});
  `;

  const result = await corrosionExec(ssh, sql);

  if (result.code !== 0) {
    throw new Error(`Failed to register container: ${result.stderr}`);
  }
}

/**
 * Unregister a container from Corrosion database
 *
 * @param ssh - SSH connection to the server
 * @param containerId - Container ID to remove
 */
export async function unregisterContainer(
  ssh: SSHManager,
  containerId: string,
): Promise<void> {
  const sql = `DELETE FROM containers WHERE id = '${escapeSql(containerId)}';`;

  const result = await corrosionExec(ssh, sql);

  if (result.code !== 0) {
    throw new Error(`Failed to unregister container: ${result.stderr}`);
  }
}

/**
 * Query containers for a service
 *
 * @param ssh - SSH connection to the server
 * @param serviceName - Service name to query
 * @returns Array of container IPs
 */
export async function queryServiceContainers(
  ssh: SSHManager,
  serviceName: string,
): Promise<string[]> {
  const sql = `SELECT ip FROM containers WHERE service = '${
    escapeSql(serviceName)
  }' AND healthy = 1;`;

  const result = await corrosionQuery(ssh, sql);

  if (result.code !== 0) {
    throw new Error(`Failed to query service containers: ${result.stderr}`);
  }

  // Parse output (one IP per line)
  return result.stdout.trim().split("\n").filter((ip: string) => ip.length > 0);
}

/**
 * Query containers for a service on a specific server
 *
 * @param ssh - SSH connection to the server
 * @param serviceName - Service name to query
 * @param serverId - Server ID to filter by
 * @returns Array of container IPs on that server
 */
export async function queryServerServiceContainers(
  ssh: SSHManager,
  serviceName: string,
  serverId: string,
): Promise<string[]> {
  const sql = `SELECT ip FROM containers WHERE service = '${
    escapeSql(serviceName)
  }' AND server_id = '${escapeSql(serverId)}' AND healthy = 1;`;

  const result = await corrosionQuery(ssh, sql);

  if (result.code !== 0) {
    throw new Error(
      `Failed to query server service containers: ${result.stderr}`,
    );
  }

  // Parse output (one IP per line)
  return result.stdout.trim().split("\n").filter((ip: string) => ip.length > 0);
}

/**
 * Query container details for a specific service on a specific server
 *
 * Returns container information including IP and instance_id (if set)
 *
 * @param ssh - SSH connection
 * @param serviceName - Service name to query
 * @param serverId - Server ID to filter by
 * @returns Array of container details with IP and instance_id
 */
export async function queryServerServiceContainerDetails(
  ssh: SSHManager,
  serviceName: string,
  serverId: string,
): Promise<Array<{ ip: string; instanceId?: string }>> {
  const sql = `SELECT ip, instance_id FROM containers WHERE service = '${
    escapeSql(serviceName)
  }' AND server_id = '${escapeSql(serverId)}' AND healthy = 1;`;

  const result = await corrosionQuery(ssh, sql);

  if (result.code !== 0) {
    throw new Error(
      `Failed to query server service containers: ${result.stderr}`,
    );
  }

  // Parse output (format: "ip|instance_id" per line, instance_id may be empty)
  const lines = result.stdout.trim().split("\n").filter((line: string) =>
    line.length > 0
  );
  return lines.map((line: string) => {
    const parts = line.split("|");
    return {
      ip: parts[0] || "",
      instanceId: parts[1] && parts[1].trim() !== "" ? parts[1] : undefined,
    };
  });
}

/**
 * Check if Corrosion binary is installed
 *
 * @param ssh - SSH connection to the server
 * @returns True if Corrosion is installed
 */
export async function isCorrosionInstalled(ssh: SSHManager): Promise<boolean> {
  const result = await ssh.executeCommand(
    `test -f ${CORROSION_INSTALL_DIR}/corrosion && echo "installed"`,
  );
  return result.stdout.trim() === "installed";
}

/**
 * Check if Corrosion service is running
 *
 * @param ssh - SSH connection to the server
 * @returns True if service is active
 */
export async function isCorrosionRunning(ssh: SSHManager): Promise<boolean> {
  const result = await ssh.executeCommand(
    "systemctl is-active jiji-corrosion",
  );
  return result.stdout.trim() === "active";
}

/**
 * Wait for Corrosion database to synchronize with cluster
 *
 * Prevents reading stale/empty state before replication completes.
 * This is critical to avoid the "empty machines list" bug where a newly
 * joined server would remove all peers before state sync finished.
 *
 * @param ssh - SSH connection to the server
 * @param expectedServerCount - Number of servers expected in the cluster
 * @param maxWaitSeconds - Maximum time to wait in seconds (default: 300 = 5 minutes)
 * @param pollIntervalMs - How often to check in milliseconds (default: 2000 = 2 seconds)
 */
export async function waitForCorrosionSync(
  ssh: SSHManager,
  _expectedServerCount: number,
  maxWaitSeconds: number = CORROSION_SYNC_TIMEOUT_SECONDS,
  pollIntervalMs: number = CORROSION_SYNC_POLL_INTERVAL_MS,
): Promise<void> {
  const host = ssh.getHost();
  const maxRetries = Math.floor((maxWaitSeconds * 1000) / pollIntervalMs);
  const logIntervalRetries = Math.floor(
    CORROSION_SYNC_LOG_INTERVAL_MS / pollIntervalMs,
  );

  for (let i = 0; i < maxRetries; i++) {
    try {
      // Simple query to check if Corrosion is responsive
      const sql = `SELECT 1 as ready`;

      const result = await corrosionQuery(ssh, sql);

      if (result.code === 0 && result.stdout.trim() === "1") {
        return;
      }

      // Log progress every 5 seconds
      if (i % logIntervalRetries === 0 && i > 0) {
        log.debug(
          `Still waiting for Corrosion on ${host}...`,
          "corrosion",
        );
      }
    } catch (error) {
      // Ignore errors during polling, will retry
      if (i % logIntervalRetries === 0 && i > 0) {
        log.debug(
          `Corrosion query failed on ${host}, retrying: ${error}`,
          "corrosion",
        );
      }
    }

    // Wait before next poll
    await new Promise((resolve) => setTimeout(resolve, pollIntervalMs));
  }

  // Timeout reached
  throw new Error(
    `Corrosion readiness timeout on ${host} after ${maxWaitSeconds} seconds`,
  );
}

/**
 * Update server endpoints in Corrosion database
 *
 * Used to persist discovered or successful endpoints back to the distributed state
 *
 * @param ssh - SSH connection to the server
 * @param serverId - Server ID to update
 * @param endpoints - Array of endpoints in "IP:PORT" format
 */
export async function updateServerEndpoints(
  ssh: SSHManager,
  serverId: string,
  endpoints: string[],
): Promise<void> {
  const endpointsJson = JSON.stringify(endpoints);

  const sql = `
    UPDATE servers
    SET endpoints = '${endpointsJson}'
    WHERE id = '${escapeSql(serverId)}';
  `;

  const result = await corrosionExec(ssh, sql);

  if (result.code !== 0) {
    throw new Error(`Failed to update server endpoints: ${result.stderr}`);
  }

  log.debug(
    `Updated endpoints for server ${serverId}: ${endpointsJson}`,
    "corrosion",
  );
}

/**
 * Update server heartbeat timestamp
 *
 * Should be called periodically by the control loop to indicate server is alive
 *
 * @param ssh - SSH connection to the server
 * @param serverId - Server ID to update
 */
export async function updateServerHeartbeat(
  ssh: SSHManager,
  serverId: string,
): Promise<void> {
  const now = Date.now();
  const sql = `
    UPDATE servers
    SET last_seen = ${now}
    WHERE id = '${escapeSql(serverId)}';
  `;

  const result = await corrosionExec(ssh, sql);

  if (result.code !== 0) {
    throw new Error(`Failed to update server heartbeat: ${result.stderr}`);
  }
}

/**
 * Query active servers from Corrosion
 *
 * Returns servers that have sent a heartbeat within the last 5 minutes
 *
 * @param ssh - SSH connection to any server (Corrosion is distributed)
 * @returns Array of active server registrations
 */
export async function queryActiveServers(
  ssh: SSHManager,
): Promise<ServerRegistration[]> {
  const sql = `
    SELECT id, hostname, subnet, wireguard_ip, wireguard_pubkey,
           management_ip, endpoints, last_seen
    FROM servers
    WHERE last_seen > (strftime('%s', 'now') - 300) * 1000
    ORDER BY hostname;
  `;

  const result = await corrosionQuery(ssh, sql);

  if (result.code !== 0) {
    throw new Error(`Failed to query active servers: ${result.stderr}`);
  }

  // Parse output - format: id|hostname|subnet|wireguard_ip|wireguard_pubkey|management_ip|endpoints|last_seen
  const lines = result.stdout.trim().split("\n").filter((line) =>
    line.length > 0
  );

  return lines.map((line) => {
    const [
      id,
      hostname,
      subnet,
      wireguardIp,
      wireguardPublicKey,
      managementIp,
      endpoints,
      lastSeenStr,
    ] = line.split("|");

    let parsedEndpoints: string[];
    try {
      parsedEndpoints = JSON.parse(endpoints);
    } catch (_error) {
      log.warn(
        `Failed to parse endpoints JSON for server ${id}: ${endpoints}`,
        "corrosion",
      );
      parsedEndpoints = [];
    }

    return {
      id,
      hostname,
      subnet,
      wireguardIp,
      wireguardPublicKey,
      managementIp,
      endpoints: parsedEndpoints,
      lastSeen: parseInt(lastSeenStr, 10),
    };
  });
}

/**
 * Query all servers from Corrosion (including inactive ones)
 *
 * @param ssh - SSH connection to any server
 * @returns Array of all server registrations
 */
export async function queryAllServers(
  ssh: SSHManager,
): Promise<ServerRegistration[]> {
  const sql = `
    SELECT id, hostname, subnet, wireguard_ip, wireguard_pubkey,
           management_ip, endpoints, last_seen
    FROM servers
    ORDER BY hostname;
  `;

  const result = await corrosionQuery(ssh, sql);

  if (result.code !== 0) {
    throw new Error(`Failed to query all servers: ${result.stderr}`);
  }

  // Parse output - format: id|hostname|subnet|wireguard_ip|wireguard_pubkey|management_ip|endpoints|last_seen
  const lines = result.stdout.trim().split("\n").filter((line) =>
    line.length > 0
  );

  return lines.map((line) => {
    const [
      id,
      hostname,
      subnet,
      wireguardIp,
      wireguardPublicKey,
      managementIp,
      endpoints,
      lastSeenStr,
    ] = line.split("|");

    let parsedEndpoints: string[];
    try {
      parsedEndpoints = JSON.parse(endpoints);
    } catch (_error) {
      log.warn(
        `Failed to parse endpoints JSON for server ${id}: ${endpoints}`,
        "corrosion",
      );
      parsedEndpoints = [];
    }

    return {
      id,
      hostname,
      subnet,
      wireguardIp,
      wireguardPublicKey,
      managementIp,
      endpoints: parsedEndpoints,
      lastSeen: parseInt(lastSeenStr, 10),
    };
  });
}

/**
 * Set cluster metadata value
 *
 * @param ssh - SSH connection to any server
 * @param key - Metadata key
 * @param value - Metadata value
 */
export async function setClusterMetadata(
  ssh: SSHManager,
  key: string,
  value: string,
): Promise<void> {
  const sql = `
    INSERT OR REPLACE INTO cluster_metadata (key, value)
    VALUES ('${escapeSql(key)}', '${escapeSql(value)}');
  `;

  const result = await corrosionExec(ssh, sql);

  if (result.code !== 0) {
    throw new Error(`Failed to set cluster metadata: ${result.stderr}`);
  }

  log.debug(`Set cluster metadata: ${key} = ${value}`, "corrosion");
}

/**
 * Get cluster metadata value
 *
 * @param ssh - SSH connection to any server
 * @param key - Metadata key
 * @returns Metadata value or null if not found
 */
export async function getClusterMetadata(
  ssh: SSHManager,
  key: string,
): Promise<string | null> {
  const sql = `SELECT value FROM cluster_metadata WHERE key = '${
    escapeSql(key)
  }';`;

  const result = await corrosionQuery(ssh, sql);

  if (result.code !== 0) {
    throw new Error(`Failed to get cluster metadata: ${result.stderr}`);
  }

  const value = result.stdout.trim();
  return value.length > 0 ? value : null;
}

/**
 * Initialize cluster metadata
 *
 * Should be called during network setup to store cluster-wide configuration
 *
 * @param ssh - SSH connection to any server
 * @param clusterCidr - Cluster CIDR (e.g., "10.210.0.0/16")
 * @param serviceDomain - Service domain (e.g., "jiji")
 * @param discovery - Discovery method
 */
export async function initializeClusterMetadata(
  ssh: SSHManager,
  clusterCidr: string,
  serviceDomain: string,
  discovery: "static" | "corrosion",
): Promise<void> {
  await setClusterMetadata(ssh, "cluster_cidr", clusterCidr);
  await setClusterMetadata(ssh, "service_domain", serviceDomain);
  await setClusterMetadata(ssh, "discovery", discovery);
  await setClusterMetadata(ssh, "created_at", new Date().toISOString());
}

// ============================================================================
// Observability Functions (Phase 2)
// ============================================================================

/**
 * Container details with server information
 */
export interface ContainerWithDetails {
  id: string;
  service: string;
  serverId: string;
  serverHostname: string;
  ip: string;
  healthy: boolean;
  startedAt: number;
  instanceId?: string;
}

/**
 * Stale container record (unhealthy for longer than threshold)
 */
export interface StaleContainer {
  id: string;
  service: string;
  serverId: string;
  startedAt: number;
  unhealthySince: number;
}

/**
 * Offline server record
 */
export interface OfflineServer {
  id: string;
  hostname: string;
  lastSeen: number;
  containerCount: number;
}

/**
 * Database statistics
 */
export interface DbStats {
  serverCount: number;
  activeServerCount: number;
  containerCount: number;
  healthyContainerCount: number;
  unhealthyContainerCount: number;
  serviceCount: number;
}

/**
 * Query containers that have been unhealthy for longer than threshold
 *
 * @param ssh - SSH connection to the server
 * @param thresholdSeconds - Seconds since container was marked unhealthy
 * @returns Array of stale containers
 */
export async function queryStaleContainers(
  ssh: SSHManager,
  thresholdSeconds: number,
): Promise<StaleContainer[]> {
  const now = Math.floor(Date.now() / 1000);
  const threshold = now - thresholdSeconds;

  const sql = `
    SELECT id, service, server_id, started_at
    FROM containers
    WHERE healthy = 0
    AND (started_at / 1000) < ${threshold}
    ORDER BY started_at;
  `;

  const result = await corrosionQuery(ssh, sql);

  if (result.code !== 0) {
    throw new Error(`Failed to query stale containers: ${result.stderr}`);
  }

  const lines = result.stdout.trim().split("\n").filter((line) =>
    line.length > 0
  );

  return lines.map((line) => {
    const [id, service, serverId, startedAtStr] = line.split("|");
    const startedAt = parseInt(startedAtStr, 10);
    return {
      id,
      service,
      serverId,
      startedAt,
      unhealthySince: now - Math.floor(startedAt / 1000),
    };
  });
}

/**
 * Query servers that have not sent a heartbeat within threshold
 *
 * @param ssh - SSH connection to the server
 * @param thresholdMs - Milliseconds since last heartbeat
 * @returns Array of offline servers with container counts
 */
export async function queryOfflineServers(
  ssh: SSHManager,
  thresholdMs: number,
): Promise<OfflineServer[]> {
  const threshold = Date.now() - thresholdMs;

  const sql = `
    SELECT s.id, s.hostname, s.last_seen,
           (SELECT COUNT(*) FROM containers c WHERE c.server_id = s.id) as container_count
    FROM servers s
    WHERE s.last_seen < ${threshold}
    ORDER BY s.last_seen;
  `;

  const result = await corrosionQuery(ssh, sql);

  if (result.code !== 0) {
    throw new Error(`Failed to query offline servers: ${result.stderr}`);
  }

  const lines = result.stdout.trim().split("\n").filter((line) =>
    line.length > 0
  );

  return lines.map((line) => {
    const [id, hostname, lastSeenStr, containerCountStr] = line.split("|");
    return {
      id,
      hostname,
      lastSeen: parseInt(lastSeenStr, 10),
      containerCount: parseInt(containerCountStr, 10) || 0,
    };
  });
}

/**
 * Delete containers by their IDs
 *
 * @param ssh - SSH connection to the server
 * @param containerIds - Array of container IDs to delete
 * @returns Number of deleted records
 */
export async function deleteContainersByIds(
  ssh: SSHManager,
  containerIds: string[],
): Promise<number> {
  if (containerIds.length === 0) return 0;

  const escapedIds = containerIds.map((id) => `'${escapeSql(id)}'`).join(", ");
  const sql = `DELETE FROM containers WHERE id IN (${escapedIds});`;

  const result = await corrosionExec(ssh, sql);

  if (result.code !== 0) {
    throw new Error(`Failed to delete containers: ${result.stderr}`);
  }

  return containerIds.length;
}

/**
 * Delete all containers for a specific server
 *
 * @param ssh - SSH connection to the server
 * @param serverId - Server ID whose containers should be deleted
 * @returns Number of deleted records
 */
export async function deleteContainersByServer(
  ssh: SSHManager,
  serverId: string,
): Promise<number> {
  // First count how many will be deleted
  const countSql = `SELECT COUNT(*) FROM containers WHERE server_id = '${
    escapeSql(serverId)
  }';`;

  const countResult = await corrosionQuery(ssh, countSql);
  const count = parseInt(countResult.stdout.trim(), 10) || 0;

  if (count === 0) return 0;

  // Delete the containers
  const sql = `DELETE FROM containers WHERE server_id = '${
    escapeSql(serverId)
  }';`;

  const result = await corrosionExec(ssh, sql);

  if (result.code !== 0) {
    throw new Error(`Failed to delete containers for server: ${result.stderr}`);
  }

  return count;
}

/**
 * Query all containers with full details including server hostname
 *
 * @param ssh - SSH connection to the server
 * @param serviceFilter - Optional service name to filter by
 * @returns Array of containers with details
 */
export async function queryAllContainersWithDetails(
  ssh: SSHManager,
  serviceFilter?: string,
): Promise<ContainerWithDetails[]> {
  let sql = `
    SELECT c.id, c.service, c.server_id, s.hostname, c.ip, c.healthy, c.started_at, c.instance_id
    FROM containers c
    LEFT JOIN servers s ON c.server_id = s.id
  `;

  if (serviceFilter) {
    sql += ` WHERE c.service = '${escapeSql(serviceFilter)}'`;
  }

  sql += " ORDER BY c.service, s.hostname;";

  const result = await corrosionQuery(ssh, sql);

  if (result.code !== 0) {
    throw new Error(`Failed to query containers: ${result.stderr}`);
  }

  const lines = result.stdout.trim().split("\n").filter((line) =>
    line.length > 0
  );

  return lines.map((line) => {
    const [
      id,
      service,
      serverId,
      serverHostname,
      ip,
      healthyStr,
      startedAtStr,
      instanceId,
    ] = line.split("|");
    return {
      id,
      service,
      serverId,
      serverHostname: serverHostname || "unknown",
      ip,
      healthy: healthyStr === "1",
      startedAt: parseInt(startedAtStr, 10),
      instanceId: instanceId && instanceId.trim() !== ""
        ? instanceId
        : undefined,
    };
  });
}

/**
 * Query a specific container by ID (supports partial matching)
 *
 * @param ssh - SSH connection to the server
 * @param containerId - Container ID (full or partial)
 * @returns Container details or null if not found
 */
export async function queryContainerById(
  ssh: SSHManager,
  containerId: string,
): Promise<ContainerWithDetails | null> {
  const sql = `
    SELECT c.id, c.service, c.server_id, s.hostname, c.ip, c.healthy, c.started_at, c.instance_id
    FROM containers c
    LEFT JOIN servers s ON c.server_id = s.id
    WHERE c.id LIKE '${escapeSql(containerId)}%'
    LIMIT 1;
  `;

  const result = await corrosionQuery(ssh, sql);

  if (result.code !== 0) {
    throw new Error(`Failed to query container: ${result.stderr}`);
  }

  const line = result.stdout.trim();
  if (!line) return null;

  const [
    id,
    service,
    serverId,
    serverHostname,
    ip,
    healthyStr,
    startedAtStr,
    instanceId,
  ] = line.split("|");

  return {
    id,
    service,
    serverId,
    serverHostname: serverHostname || "unknown",
    ip,
    healthy: healthyStr === "1",
    startedAt: parseInt(startedAtStr, 10),
    instanceId: instanceId && instanceId.trim() !== "" ? instanceId : undefined,
  };
}

/**
 * Execute arbitrary SQL query against Corrosion
 *
 * @param ssh - SSH connection to the server
 * @param sql - SQL query to execute
 * @returns Raw query output
 */
export async function executeSql(
  ssh: SSHManager,
  sql: string,
): Promise<string> {
  const result = await corrosionQuery(ssh, sql);

  if (result.code !== 0) {
    throw new Error(`SQL query failed: ${result.stderr}`);
  }

  return result.stdout;
}

/**
 * Get database statistics
 *
 * @param ssh - SSH connection to the server
 * @returns Database statistics
 */
export async function getDbStats(ssh: SSHManager): Promise<DbStats> {
  const now = Date.now();
  const activeThreshold = now - 300000; // 5 minutes

  const sql = `
    SELECT
      (SELECT COUNT(*) FROM servers) as server_count,
      (SELECT COUNT(*) FROM servers WHERE last_seen > ${activeThreshold}) as active_server_count,
      (SELECT COUNT(*) FROM containers) as container_count,
      (SELECT COUNT(*) FROM containers WHERE healthy = 1) as healthy_container_count,
      (SELECT COUNT(*) FROM containers WHERE healthy = 0) as unhealthy_container_count,
      (SELECT COUNT(*) FROM services) as service_count;
  `;

  const result = await corrosionQuery(ssh, sql);

  if (result.code !== 0) {
    throw new Error(`Failed to get database stats: ${result.stderr}`);
  }

  const line = result.stdout.trim();
  const [
    serverCount,
    activeServerCount,
    containerCount,
    healthyContainerCount,
    unhealthyContainerCount,
    serviceCount,
  ] = line.split("|").map((v) => parseInt(v, 10) || 0);

  return {
    serverCount,
    activeServerCount,
    containerCount,
    healthyContainerCount,
    unhealthyContainerCount,
    serviceCount,
  };
}
