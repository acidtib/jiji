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
 * Clean up orphaned network interfaces that might conflict with WireGuard
 *
 * This removes stale bridge interfaces (like podman1, docker0) that might have
 * IP addresses or routes in the cluster CIDR range, which would conflict with
 * WireGuard route setup.
 *
 * @param ssh - SSH connection to the server
 * @param clusterCidr - Cluster CIDR to check for conflicts (e.g., "10.210.0.0/16")
 */
export async function cleanupOrphanedInterfaces(
  ssh: SSHManager,
  clusterCidr: string,
): Promise<void> {
  const host = ssh.getHost();

  // Extract base network from CIDR (e.g., "10.210" from "10.210.0.0/16")
  const baseNetwork = clusterCidr.split(".").slice(0, 2).join(".");

  // Find all bridge interfaces
  const bridgesResult = await ssh.executeCommand(
    `ip -o link show type bridge | awk -F': ' '{print $2}'`,
  );
  if (bridgesResult.code !== 0) return;

  const bridges = bridgesResult.stdout.trim().split("\n").filter((b) =>
    b && !b.includes("jiji")
  );

  for (const bridge of bridges) {
    // Check if this bridge has an IP in our cluster range
    const ipResult = await ssh.executeCommand(
      `ip -o -4 addr show ${bridge} | awk '{print $4}'`,
    );
    if (ipResult.code !== 0 || !ipResult.stdout.trim()) continue;

    const ip = ipResult.stdout.trim();
    if (ip.startsWith(baseNetwork)) {
      log.debug(
        `Found orphaned bridge ${bridge} with conflicting IP ${ip}, removing...`,
        "network",
      );

      // Bring down and delete the interface
      await ssh.executeCommand(`ip link set ${bridge} down || true`);
      await ssh.executeCommand(`ip link delete ${bridge} || true`);

      log.debug(`Removed orphaned bridge ${bridge} on ${host}`, "network");
    }
  }
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
  const _host = ssh.getHost();

  // Check if interface is already up
  const checkResult = await ssh.executeCommand(`ip link show ${interfaceName}`);
  if (checkResult.code === 0) {
    // Bring it down first
    await ssh.executeCommand(`wg-quick down ${interfaceName} || true`);
  }

  // Bring up the interface
  const upResult = await ssh.executeCommand(`wg-quick up ${interfaceName}`);
  if (upResult.code !== 0) {
    // Check if it's a route conflict error
    if (upResult.stderr.includes("RTNETLINK answers: File exists")) {
      // Extract the conflicting route if possible
      const routeMatch = upResult.stderr.match(/ip -4 route add ([0-9.\/]+)/);
      const conflictingRoute = routeMatch ? routeMatch[1] : "unknown";

      throw new Error(
        `Failed to bring up WireGuard interface: Route conflict detected for ${conflictingRoute}.\n` +
          `This usually means an orphaned network interface is using the same subnet.\n` +
          `Try running: ip route show | grep ${
            conflictingRoute.split("/")[0]
          }\n` +
          `Then remove the conflicting interface with: ip link delete <interface_name>\n` +
          `Full error: ${upResult.stderr}`,
      );
    }

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

/**
 * Verify WireGuard configuration matches expected state
 *
 * This is critical to catch cases where the config file was written but the
 * running interface doesn't reflect the configuration (e.g., after reload failures).
 *
 * @param ssh - SSH connection to the server
 * @param expectedPeers - Array of expected peer configurations
 * @param interfaceName - Interface name (default: jiji0)
 * @returns Verification result with details about any mismatches
 */
export async function verifyWireGuardConfig(
  ssh: SSHManager,
  expectedPeers: Array<{
    publicKey: string;
    allowedIps: string[];
  }>,
  interfaceName = "jiji0",
): Promise<{
  success: boolean;
  peerCountMatch: boolean;
  missingPeers: string[];
  incorrectAllowedIps: Array<
    { peer: string; expected: string[]; actual: string[] }
  >;
  details?: string;
}> {
  const host = ssh.getHost();

  // Get detailed WireGuard configuration
  const result = await ssh.executeCommand(`wg show ${interfaceName} dump`);
  if (result.code !== 0) {
    return {
      success: false,
      peerCountMatch: false,
      missingPeers: [],
      incorrectAllowedIps: [],
      details: `Failed to get WireGuard status on ${host}: ${result.stderr}`,
    };
  }

  // Parse dump output (format: interface line, then peer lines)
  // Interface: private-key public-key listen-port fwmark
  // Peer: public-key preshared-key endpoint allowed-ips latest-handshake transfer-rx transfer-tx persistent-keepalive
  const lines = result.stdout.trim().split("\n");
  const actualPeers = new Map<string, string[]>();

  for (let i = 1; i < lines.length; i++) {
    const parts = lines[i].split("\t");
    if (parts.length >= 4) {
      const publicKey = parts[0];
      const allowedIps = parts[3].split(",").map((ip) => ip.trim());
      actualPeers.set(publicKey, allowedIps);
    }
  }

  // Check peer count
  const peerCountMatch = actualPeers.size === expectedPeers.length;
  const missingPeers: string[] = [];
  const incorrectAllowedIps: Array<
    { peer: string; expected: string[]; actual: string[] }
  > = [];

  // Helper to normalize IP addresses for comparison
  // IPv6 addresses may have leading zeros stripped by WireGuard
  const normalizeIp = (ip: string): string => {
    // For IPv6, expand compressed notation and normalize
    if (ip.includes(":")) {
      // Remove /128 suffix for comparison
      const [addr, suffix] = ip.split("/");
      // Normalize by expanding :: and padding segments
      const segments = addr.split(":");
      const normalized = segments.map((seg) => {
        // Pad each segment to 4 chars, handling empty segments from ::
        return seg.length > 0 ? seg.padStart(4, "0") : "0000";
      }).join(":");
      return suffix ? `${normalized}/${suffix}` : normalized;
    }
    return ip;
  };

  // Verify each expected peer
  for (const expectedPeer of expectedPeers) {
    const actualAllowedIps = actualPeers.get(expectedPeer.publicKey);

    if (!actualAllowedIps) {
      missingPeers.push(expectedPeer.publicKey);
      continue;
    }

    // Normalize and sort both arrays for comparison
    const expectedSorted = [...expectedPeer.allowedIps].map(normalizeIp).sort();
    const actualSorted = [...actualAllowedIps].map(normalizeIp).sort();

    // Check if AllowedIPs match
    const allowedIpsMatch = expectedSorted.length === actualSorted.length &&
      expectedSorted.every((ip, idx) => ip === actualSorted[idx]);

    if (!allowedIpsMatch) {
      incorrectAllowedIps.push({
        peer: expectedPeer.publicKey.substring(0, 8) + "...",
        expected: expectedSorted,
        actual: actualSorted,
      });
    }
  }

  const success = peerCountMatch &&
    missingPeers.length === 0 &&
    incorrectAllowedIps.length === 0;

  return {
    success,
    peerCountMatch,
    missingPeers,
    incorrectAllowedIps,
  };
}
