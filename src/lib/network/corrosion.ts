/**
 * Corrosion distributed database service
 *
 * Handles installation, configuration, and interaction with Corrosion
 * for service discovery and state management.
 */

import type { SSHManager } from "../../utils/ssh.ts";
import { log } from "../../utils/logger.ts";
import type {
  ContainerRegistration,
  CorrosionConfig,
  ServerRegistration,
  ServiceRegistration,
} from "../../types/network.ts";

const CORROSION_REPO = "psviderski/corrosion";
const CORROSION_INSTALL_DIR = "/opt/jiji/corrosion";

/**
 * Database schema for Corrosion
 */
const CORROSION_SCHEMA = `-- Jiji network database schema

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
  started_at INTEGER NOT NULL DEFAULT 0
);

-- Enable CRDT for all tables (disabled for compatibility)
-- SELECT crsql_as_crr('servers');
-- SELECT crsql_as_crr('services');
-- SELECT crsql_as_crr('containers');
`;

/**
 * Install Corrosion on a remote server
 *
 * @param ssh - SSH connection to the server
 * @returns True if installation was successful
 */
export async function installCorrosion(ssh: SSHManager): Promise<boolean> {
  const host = ssh.getHost();
  log.info(`Installing Corrosion on ${host}`, "corrosion");

  // Check if Corrosion is already installed
  const checkResult = await ssh.executeCommand(
    `test -f ${CORROSION_INSTALL_DIR}/corrosion && echo "exists"`,
  );

  if (checkResult.stdout.includes("exists")) {
    log.success(`Corrosion already installed on ${host}`, "corrosion");
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

    log.info(`Downloading Corrosion for ${downloadArch}...`, "corrosion");

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

    log.success(`Corrosion installed successfully on ${host}`, "corrosion");
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
  const bootstrapPeers = config.bootstrap
    .map((peer) => `"${peer}"`)
    .join(", ");

  return `[db]
path = "${config.dbPath}"
schema_paths = ["${config.schemaPath}"]

[gossip]
addr = "${config.gossipAddr}"
bootstrap = [${bootstrapPeers}]
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

  log.success("Corrosion configuration written", "corrosion");
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

  log.success("Corrosion systemd service created", "corrosion");
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

  log.success("Corrosion service started", "corrosion");
}

/**
 * Stop Corrosion service
 *
 * @param ssh - SSH connection to the server
 */
export async function stopCorrosionService(ssh: SSHManager): Promise<void> {
  await ssh.executeCommand("systemctl stop jiji-corrosion");
  log.success("Corrosion service stopped", "corrosion");
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
  const sql = `
    INSERT OR REPLACE INTO servers
    (id, hostname, subnet, wireguard_ip, wireguard_pubkey, management_ip, endpoints, last_seen)
    VALUES
    ('${server.id}', '${server.hostname}', '${server.subnet}', '${server.wireguardIp}',
     '${server.wireguardPublicKey}', '${server.managementIp}', '${server.endpoints}', ${server.lastSeen});
  `;

  const result = await ssh.executeCommand(
    `${CORROSION_INSTALL_DIR}/corrosion exec --config ${CORROSION_INSTALL_DIR}/config.toml "${
      sql.replace(/\n/g, " ")
    }"`,
  );

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
    VALUES ('${service.name}', '${service.project}');
  `;

  const result = await ssh.executeCommand(
    `${CORROSION_INSTALL_DIR}/corrosion exec --config ${CORROSION_INSTALL_DIR}/config.toml "${
      sql.replace(/\n/g, " ")
    }"`,
  );

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

  const sql = `
    INSERT OR REPLACE INTO containers
    (id, service, server_id, ip, healthy, started_at)
    VALUES
    ('${container.id}', '${container.service}', '${container.serverId}',
     '${container.ip}', ${healthy}, ${container.startedAt});
  `;

  const result = await ssh.executeCommand(
    `${CORROSION_INSTALL_DIR}/corrosion exec --config ${CORROSION_INSTALL_DIR}/config.toml "${
      sql.replace(/\n/g, " ")
    }"`,
  );

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
  const sql = `DELETE FROM containers WHERE id = '${containerId}';`;

  const result = await ssh.executeCommand(
    `${CORROSION_INSTALL_DIR}/corrosion exec --config ${CORROSION_INSTALL_DIR}/config.toml "${sql}"`,
  );

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
  const sql =
    `SELECT ip FROM containers WHERE service = '${serviceName}' AND healthy = 1;`;

  const result = await ssh.executeCommand(
    `${CORROSION_INSTALL_DIR}/corrosion exec --config ${CORROSION_INSTALL_DIR}/config.toml "${sql}"`,
  );

  if (result.code !== 0) {
    throw new Error(`Failed to query service containers: ${result.stderr}`);
  }

  // Parse output (one IP per line)
  return result.stdout.trim().split("\n").filter((ip: string) => ip.length > 0);
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
