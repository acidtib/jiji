/**
 * Daemon binary installation and service management
 *
 * Downloads and installs the jiji-daemon binary from GitHub releases,
 * configures it as a systemd service with environment variables.
 *
 * The daemon runs continuously on each server and handles:
 * 1. Topology reconciliation - Add/remove WireGuard peers based on Corrosion state
 * 2. Endpoint health monitoring - Check handshake times and rotate endpoints
 * 3. Container health tracking - Validate containers and update Corrosion
 * 4. Heartbeat updates - Keep server alive in Corrosion
 * 5. Public IP discovery - Periodically refresh endpoints
 */

import type { SSHManager } from "../../utils/ssh.ts";
import { log } from "../../utils/logger.ts";
import { CORROSION_API_PORT } from "../../constants.ts";
import { VERSION } from "../../version.ts";

const DAEMON_INSTALL_DIR = "/opt/jiji/bin";
const DAEMON_BINARY = "jiji-daemon";
const DAEMON_REPO = "acidtib/jiji";
const DAEMON_VERSION = VERSION;

/**
 * Install jiji-daemon binary on a remote server
 *
 * Downloads pre-built binary from GitHub releases.
 *
 * @param ssh - SSH connection to the server
 * @returns True if installation was successful
 */
export async function installDaemon(
  ssh: SSHManager,
): Promise<boolean> {
  const host = ssh.getHost();
  const binaryPath = `${DAEMON_INSTALL_DIR}/${DAEMON_BINARY}`;

  // Clean up old jiji-control-loop binary and service (backward compatibility)
  await ssh.executeCommand(
    `rm -f ${DAEMON_INSTALL_DIR}/jiji-control-loop`,
  );
  await ssh.executeCommand(
    "systemctl stop jiji-control-loop.service 2>/dev/null || true",
  );
  await ssh.executeCommand(
    "systemctl disable jiji-control-loop.service 2>/dev/null || true",
  );
  await ssh.executeCommand(
    "rm -f /etc/systemd/system/jiji-control-loop.service",
  );

  // Check if already installed
  const checkResult = await ssh.executeCommand(
    `test -f ${binaryPath} && echo "exists"`,
  );

  if (checkResult.stdout.includes("exists")) {
    log.debug(`jiji-daemon already installed on ${host}`, "network");
    return true;
  }

  // Create installation directory
  await ssh.executeCommand(`mkdir -p ${DAEMON_INSTALL_DIR}`);

  // Detect architecture
  const archResult = await ssh.executeCommand("uname -m");
  const arch = archResult.stdout.trim();

  let downloadArch: string;
  if (arch === "x86_64" || arch === "amd64") {
    downloadArch = "linux-x64";
  } else if (arch === "aarch64" || arch === "arm64") {
    downloadArch = "linux-arm64";
  } else {
    log.error(`Unsupported architecture: ${arch}`, "network");
    return false;
  }

  try {
    const downloadUrl =
      `https://github.com/${DAEMON_REPO}/releases/download/v${DAEMON_VERSION}/${DAEMON_BINARY}-${downloadArch}`;

    log.info(
      `Downloading jiji-daemon from ${downloadUrl}...`,
      "network",
    );

    const downloadResult = await ssh.executeCommand(
      `cd ${DAEMON_INSTALL_DIR} && curl -fsSL "${downloadUrl}" -o ${DAEMON_BINARY}`,
    );

    if (downloadResult.code !== 0) {
      throw new Error(
        `Failed to download jiji-daemon: ${downloadResult.stderr}`,
      );
    }

    // Make executable
    await ssh.executeCommand(`chmod +x ${binaryPath}`);

    log.success(`jiji-daemon installed on ${host}`, "network");
    return true;
  } catch (error) {
    log.error(
      `Failed to install jiji-daemon on ${host}: ${error}`,
      "network",
    );
    return false;
  }
}

/**
 * Create systemd service for the daemon
 *
 * @param ssh - SSH connection to the server
 * @param serverId - ID of the local server
 * @param engine - Container engine (docker or podman)
 * @param interfaceName - WireGuard interface name (default: jiji0)
 */
export async function createDaemonService(
  ssh: SSHManager,
  serverId: string,
  engine: "docker" | "podman",
  interfaceName: string = "jiji0",
): Promise<void> {
  const binaryPath = `${DAEMON_INSTALL_DIR}/${DAEMON_BINARY}`;

  // Install binary if needed
  const installed = await installDaemon(ssh);
  if (!installed) {
    throw new Error("Failed to install jiji-daemon binary");
  }

  // Create systemd service with environment variables
  const serviceContent = `[Unit]
Description=Jiji Network Daemon
After=jiji-corrosion.service
Requires=jiji-corrosion.service

[Service]
Type=simple
ExecStart=${binaryPath}
Environment="JIJI_SERVER_ID=${serverId}"
Environment="JIJI_ENGINE=${engine}"
Environment="JIJI_INTERFACE=${interfaceName}"
Environment="JIJI_CORROSION_API=http://127.0.0.1:${CORROSION_API_PORT}"
Environment="JIJI_CORROSION_DIR=/opt/jiji/corrosion"
Environment="JIJI_LOOP_INTERVAL=30"
Restart=always
RestartSec=10
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
`;

  const serviceResult = await ssh.executeCommand(
    `cat > /etc/systemd/system/jiji-daemon.service << 'EOFSERVICE'\n${serviceContent}\nEOFSERVICE`,
  );

  if (serviceResult.code !== 0) {
    throw new Error(
      `Failed to create daemon service: ${serviceResult.stderr}`,
    );
  }

  // Reload systemd
  await ssh.executeCommand("systemctl daemon-reload");

  // Enable and start service
  await ssh.executeCommand("systemctl enable jiji-daemon.service");
  await ssh.executeCommand("systemctl restart jiji-daemon.service");
}

/**
 * Stop and remove daemon service
 *
 * @param ssh - SSH connection to the server
 */
export async function removeDaemonService(
  ssh: SSHManager,
): Promise<void> {
  await ssh.executeCommand(
    "systemctl stop jiji-daemon.service 2>/dev/null || true",
  );
  await ssh.executeCommand(
    "systemctl disable jiji-daemon.service 2>/dev/null || true",
  );
  await ssh.executeCommand(
    "rm -f /etc/systemd/system/jiji-daemon.service",
  );
  await ssh.executeCommand(
    `rm -f ${DAEMON_INSTALL_DIR}/${DAEMON_BINARY}`,
  );
  // Also clean up old control-loop files from prior version
  await ssh.executeCommand(
    `rm -f ${DAEMON_INSTALL_DIR}/jiji-control-loop`,
  );
  await ssh.executeCommand(
    `rm -f ${DAEMON_INSTALL_DIR}/jiji-control-loop.sh`,
  );
  await ssh.executeCommand(
    "rm -f /etc/systemd/system/jiji-control-loop.service",
  );
  await ssh.executeCommand("systemctl daemon-reload");

  log.debug("Daemon service removed", "network");
}
