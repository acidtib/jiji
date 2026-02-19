/**
 * Control loop binary installation and service management
 *
 * Downloads and installs the jiji-control-loop binary from GitHub releases,
 * configures it as a systemd service with environment variables.
 *
 * The control loop runs continuously on each server and handles:
 * 1. Topology reconciliation - Add/remove WireGuard peers based on Corrosion state
 * 2. Endpoint health monitoring - Check handshake times and rotate endpoints
 * 3. Container health tracking - Validate containers and update Corrosion
 * 4. Heartbeat updates - Keep server alive in Corrosion
 * 5. Public IP discovery - Periodically refresh endpoints
 */

import type { SSHManager } from "../../utils/ssh.ts";
import { log } from "../../utils/logger.ts";
import { CORROSION_API_PORT } from "../../constants.ts";

const CONTROL_LOOP_INSTALL_DIR = "/opt/jiji/bin";
const CONTROL_LOOP_BINARY = "jiji-control-loop";
const CONTROL_LOOP_REPO = "acidtib/jiji-control-loop";
const CONTROL_LOOP_VERSION = "0.1.0";

/**
 * Install jiji-control-loop binary on a remote server
 *
 * Downloads pre-built binary from GitHub releases.
 *
 * @param ssh - SSH connection to the server
 * @returns True if installation was successful
 */
export async function installControlLoop(
  ssh: SSHManager,
): Promise<boolean> {
  const host = ssh.getHost();
  const binaryPath = `${CONTROL_LOOP_INSTALL_DIR}/${CONTROL_LOOP_BINARY}`;

  // Check if already installed
  const checkResult = await ssh.executeCommand(
    `test -f ${binaryPath} && echo "exists"`,
  );

  if (checkResult.stdout.includes("exists")) {
    log.debug(`jiji-control-loop already installed on ${host}`, "network");
    return true;
  }

  // Create installation directory
  await ssh.executeCommand(`mkdir -p ${CONTROL_LOOP_INSTALL_DIR}`);

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
      `https://github.com/${CONTROL_LOOP_REPO}/releases/download/v${CONTROL_LOOP_VERSION}/${CONTROL_LOOP_BINARY}-${downloadArch}`;

    log.info(
      `Downloading jiji-control-loop from ${downloadUrl}...`,
      "network",
    );

    const downloadResult = await ssh.executeCommand(
      `cd ${CONTROL_LOOP_INSTALL_DIR} && curl -fsSL "${downloadUrl}" -o ${CONTROL_LOOP_BINARY}`,
    );

    if (downloadResult.code !== 0) {
      throw new Error(
        `Failed to download jiji-control-loop: ${downloadResult.stderr}`,
      );
    }

    // Make executable
    await ssh.executeCommand(`chmod +x ${binaryPath}`);

    log.success(`jiji-control-loop installed on ${host}`, "network");
    return true;
  } catch (error) {
    log.error(
      `Failed to install jiji-control-loop on ${host}: ${error}`,
      "network",
    );
    return false;
  }
}

/**
 * Create systemd service for the control loop
 *
 * @param ssh - SSH connection to the server
 * @param serverId - ID of the local server
 * @param engine - Container engine (docker or podman)
 * @param interfaceName - WireGuard interface name (default: jiji0)
 */
export async function createControlLoopService(
  ssh: SSHManager,
  serverId: string,
  engine: "docker" | "podman",
  interfaceName: string = "jiji0",
): Promise<void> {
  const binaryPath = `${CONTROL_LOOP_INSTALL_DIR}/${CONTROL_LOOP_BINARY}`;

  // Install binary if needed
  const installed = await installControlLoop(ssh);
  if (!installed) {
    throw new Error("Failed to install jiji-control-loop binary");
  }

  // Create systemd service with environment variables
  const serviceContent = `[Unit]
Description=Jiji Network Control Loop
After=jiji-corrosion.service
Requires=jiji-corrosion.service

[Service]
Type=simple
ExecStart=${binaryPath}
Environment="SERVER_ID=${serverId}"
Environment="ENGINE=${engine}"
Environment="INTERFACE=${interfaceName}"
Environment="CORROSION_API=http://127.0.0.1:${CORROSION_API_PORT}"
Environment="CORROSION_DIR=/opt/jiji/corrosion"
Environment="LOOP_INTERVAL=30"
Restart=always
RestartSec=10
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
`;

  const serviceResult = await ssh.executeCommand(
    `cat > /etc/systemd/system/jiji-control-loop.service << 'EOFSERVICE'\n${serviceContent}\nEOFSERVICE`,
  );

  if (serviceResult.code !== 0) {
    throw new Error(
      `Failed to create control loop service: ${serviceResult.stderr}`,
    );
  }

  // Reload systemd
  await ssh.executeCommand("systemctl daemon-reload");

  // Enable and start service
  await ssh.executeCommand("systemctl enable jiji-control-loop.service");
  await ssh.executeCommand("systemctl restart jiji-control-loop.service");
}

/**
 * Stop and remove control loop service
 *
 * @param ssh - SSH connection to the server
 */
export async function removeControlLoopService(
  ssh: SSHManager,
): Promise<void> {
  await ssh.executeCommand(
    "systemctl stop jiji-control-loop.service 2>/dev/null || true",
  );
  await ssh.executeCommand(
    "systemctl disable jiji-control-loop.service 2>/dev/null || true",
  );
  await ssh.executeCommand(
    "rm -f /etc/systemd/system/jiji-control-loop.service",
  );
  await ssh.executeCommand(
    `rm -f ${CONTROL_LOOP_INSTALL_DIR}/${CONTROL_LOOP_BINARY}`,
  );
  // Also clean up old bash script if it exists from prior version
  await ssh.executeCommand(
    `rm -f ${CONTROL_LOOP_INSTALL_DIR}/jiji-control-loop.sh`,
  );
  await ssh.executeCommand("systemctl daemon-reload");

  log.debug("Control loop service removed", "network");
}
