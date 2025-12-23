/**
 * WireGuard configuration generator and utilities
 */

import type { WireGuardConfig } from "../../types/network.ts";
import type { SSHManager } from "../../utils/ssh.ts";
import { log } from "../../utils/logger.ts";

/**
 * Generate a WireGuard private/public keypair on a remote server
 *
 * @param ssh - SSH connection to the server
 * @returns Object with privateKey and publicKey
 */
export async function generateWireGuardKeypair(
  ssh: SSHManager,
): Promise<{ privateKey: string; publicKey: string }> {
  // Generate private key
  const privateKeyResult = await ssh.executeCommand("wg genkey");
  if (privateKeyResult.code !== 0) {
    throw new Error(
      `Failed to generate WireGuard private key: ${privateKeyResult.stderr}`,
    );
  }

  const privateKey = privateKeyResult.stdout.trim();

  // Generate public key from private key
  const publicKeyResult = await ssh.executeCommand(
    `echo "${privateKey}" | wg pubkey`,
  );
  if (publicKeyResult.code !== 0) {
    throw new Error(
      `Failed to generate WireGuard public key: ${publicKeyResult.stderr}`,
    );
  }

  const publicKey = publicKeyResult.stdout.trim();

  return { privateKey, publicKey };
}

/**
 * Generate a WireGuard configuration file content
 *
 * @param config - WireGuard configuration object
 * @returns Configuration file content as string
 */
export function generateWireGuardConfig(config: WireGuardConfig): string {
  const lines: string[] = [];

  // [Interface] section
  lines.push("[Interface]");
  lines.push(`PrivateKey = ${config.privateKey}`);

  // Add addresses
  for (const addr of config.address) {
    lines.push(`Address = ${addr}`);
  }

  lines.push(`ListenPort = ${config.listenPort}`);

  // Add MTU if specified (recommended: 1420 for WireGuard to avoid fragmentation)
  if (config.mtu) {
    lines.push(`MTU = ${config.mtu}`);
  }

  lines.push("");

  // [Peer] sections
  for (const peer of config.peers) {
    lines.push("[Peer]");
    lines.push(`PublicKey = ${peer.publicKey}`);

    // Add allowed IPs
    for (const allowedIp of peer.allowedIps) {
      lines.push(`AllowedIPs = ${allowedIp}`);
    }

    if (peer.endpoint) {
      lines.push(`Endpoint = ${peer.endpoint}`);
    }

    if (peer.persistentKeepalive) {
      lines.push(`PersistentKeepalive = ${peer.persistentKeepalive}`);
    }

    lines.push("");
  }

  return lines.join("\n");
}

/**
 * Install WireGuard on a remote server
 *
 * @param ssh - SSH connection to the server
 * @returns True if installation was successful
 */
export async function installWireGuard(ssh: SSHManager): Promise<boolean> {
  const host = ssh.getHost();

  // Check if WireGuard is already installed
  const checkResult = await ssh.executeCommand("which wg");
  if (checkResult.code === 0) {
    return true;
  }

  // Detect OS and install accordingly
  const osResult = await ssh.executeCommand("cat /etc/os-release");
  if (osResult.code !== 0) {
    throw new Error(`Failed to detect OS on ${host}`);
  }

  const osRelease = osResult.stdout.toLowerCase();

  try {
    if (osRelease.includes("ubuntu") || osRelease.includes("debian")) {
      // Ubuntu/Debian
      await ssh.executeCommand(
        "DEBIAN_FRONTEND=noninteractive apt-get update -qq",
      );
      const installResult = await ssh.executeCommand(
        "DEBIAN_FRONTEND=noninteractive apt-get install -y -qq wireguard-tools",
      );
      if (installResult.code !== 0) {
        throw new Error(
          `Failed to install WireGuard: ${installResult.stderr}`,
        );
      }
    } else if (
      osRelease.includes("centos") || osRelease.includes("rhel") ||
      osRelease.includes("fedora")
    ) {
      // CentOS/RHEL/Fedora
      const installResult = await ssh.executeCommand(
        "yum install -y wireguard-tools || dnf install -y wireguard-tools",
      );
      if (installResult.code !== 0) {
        throw new Error(
          `Failed to install WireGuard: ${installResult.stderr}`,
        );
      }
    } else if (osRelease.includes("arch")) {
      // Arch Linux
      const installResult = await ssh.executeCommand(
        "pacman -Sy --noconfirm wireguard-tools",
      );
      if (installResult.code !== 0) {
        throw new Error(
          `Failed to install WireGuard: ${installResult.stderr}`,
        );
      }
    } else {
      throw new Error(
        `Unsupported OS on ${host}. Please install WireGuard manually.`,
      );
    }

    log.success(`WireGuard installed successfully on ${host}`, "wireguard");
    return true;
  } catch (error) {
    log.error(
      `Failed to install WireGuard on ${host}: ${error}`,
      "wireguard",
    );
    return false;
  }
}

/**
 * Write WireGuard configuration to a remote server
 *
 * @param ssh - SSH connection to the server
 * @param config - WireGuard configuration
 * @param interfaceName - Interface name (default: jiji0)
 */
export async function writeWireGuardConfig(
  ssh: SSHManager,
  config: WireGuardConfig,
  interfaceName = "jiji0",
): Promise<void> {
  const configContent = generateWireGuardConfig(config);
  const configPath = `/etc/wireguard/${interfaceName}.conf`;

  // Create wireguard directory if it doesn't exist
  await ssh.executeCommand("mkdir -p /etc/wireguard");

  // Write config file
  const writeResult = await ssh.executeCommand(
    `cat > ${configPath} << 'EOFWG'\n${configContent}\nEOFWG`,
  );

  if (writeResult.code !== 0) {
    throw new Error(
      `Failed to write WireGuard config: ${writeResult.stderr}`,
    );
  }

  // Set proper permissions (600)
  await ssh.executeCommand(`chmod 600 ${configPath}`);
}

/**
 * Bring up a WireGuard interface
 *
 * @param ssh - SSH connection to the server
 * @param interfaceName - Interface name (default: jiji0)
 */
export async function bringUpWireGuardInterface(
  ssh: SSHManager,
  interfaceName = "jiji0",
): Promise<void> {
  const host = ssh.getHost();

  // Check if interface is already up
  const checkResult = await ssh.executeCommand(`ip link show ${interfaceName}`);
  if (checkResult.code === 0) {
    // Bring it down first
    await ssh.executeCommand(`wg-quick down ${interfaceName} || true`);
  }

  // Bring up the interface
  const upResult = await ssh.executeCommand(`wg-quick up ${interfaceName}`);
  if (upResult.code !== 0) {
    throw new Error(
      `Failed to bring up WireGuard interface: ${upResult.stderr}`,
    );
  }
}

/**
 * Enable WireGuard interface to start on boot
 *
 * @param ssh - SSH connection to the server
 * @param interfaceName - Interface name (default: jiji0)
 */
export async function enableWireGuardService(
  ssh: SSHManager,
  interfaceName = "jiji0",
): Promise<void> {
  const result = await ssh.executeCommand(
    `systemctl enable wg-quick@${interfaceName}`,
  );

  if (result.code !== 0) {
    throw new Error(
      `Failed to enable WireGuard service: ${result.stderr}`,
    );
  }
}

/**
 * Bring down a WireGuard interface
 *
 * @param ssh - SSH connection to the server
 * @param interfaceName - Interface name (default: jiji0)
 */
export async function bringDownWireGuardInterface(
  ssh: SSHManager,
  interfaceName = "jiji0",
): Promise<void> {
  const result = await ssh.executeCommand(`wg-quick down ${interfaceName}`);
  if (result.code !== 0) {
    // Don't throw error if interface doesn't exist
    if (!result.stderr.includes("does not exist")) {
      throw new Error(
        `Failed to bring down WireGuard interface: ${result.stderr}`,
      );
    }
  }

  log.success(`WireGuard interface ${interfaceName} is down`, "wireguard");
}

/**
 * Disable WireGuard service
 *
 * @param ssh - SSH connection to the server
 * @param interfaceName - Interface name (default: jiji0)
 */
export async function disableWireGuardService(
  ssh: SSHManager,
  interfaceName = "jiji0",
): Promise<void> {
  await ssh.executeCommand(
    `systemctl disable wg-quick@${interfaceName} || true`,
  );
  log.success(`WireGuard service disabled for ${interfaceName}`, "wireguard");
}

/**
 * Restart WireGuard interface to pick up configuration changes
 *
 * @param ssh - SSH connection to the server
 * @param interfaceName - Interface name (default: jiji0)
 */
export async function restartWireGuardInterface(
  ssh: SSHManager,
  interfaceName = "jiji0",
): Promise<void> {
  const host = ssh.getHost();

  log.info(
    `Restarting WireGuard interface ${interfaceName} on ${host}`,
    "wireguard",
  );

  // Bring down the interface (ignore errors if it's not up)
  await ssh.executeCommand(`wg-quick down ${interfaceName} || true`);

  // Bring up the interface with new configuration
  const upResult = await ssh.executeCommand(`wg-quick up ${interfaceName}`);
  if (upResult.code !== 0) {
    throw new Error(
      `Failed to restart WireGuard interface: ${upResult.stderr}`,
    );
  }

  log.success(
    `WireGuard interface ${interfaceName} restarted on ${host}`,
    "wireguard",
  );
}

/**
 * Get WireGuard interface status
 *
 * @param ssh - SSH connection to the server
 * @param interfaceName - Interface name (default: jiji0)
 * @returns Interface status information
 */
export async function getWireGuardStatus(
  ssh: SSHManager,
  interfaceName = "jiji0",
): Promise<{
  exists: boolean;
  up: boolean;
  publicKey?: string;
  peers?: number;
}> {
  // Check if interface exists
  const checkResult = await ssh.executeCommand(`ip link show ${interfaceName}`);
  if (checkResult.code !== 0) {
    return { exists: false, up: false };
  }

  // Get interface details
  const statusResult = await ssh.executeCommand(`wg show ${interfaceName}`);
  if (statusResult.code !== 0) {
    return { exists: true, up: false };
  }

  const output = statusResult.stdout;

  // Parse public key
  const pubkeyMatch = output.match(/public key: (.+)/);
  const publicKey = pubkeyMatch ? pubkeyMatch[1].trim() : undefined;

  // Count peers
  const peerMatches = output.match(/peer: /g);
  const peers = peerMatches ? peerMatches.length : 0;

  return {
    exists: true,
    up: true,
    publicKey,
    peers,
  };
}
