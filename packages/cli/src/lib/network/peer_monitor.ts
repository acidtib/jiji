/**
 * WireGuard peer health monitoring
 *
 * Monitors WireGuard peer connection status and performs automatic
 * endpoint rotation when peers become unreachable.
 */

import type { SSHManager } from "../../utils/ssh.ts";
import type { NetworkServer } from "../../types/network.ts";
import { log } from "../../utils/logger.ts";

/**
 * WireGuard peer status from device
 */
interface PeerStatus {
  publicKey: string;
  endpoint?: string;
  lastHandshake: number; // seconds since last handshake
  rxBytes: number;
  txBytes: number;
  allowedIps: string[];
}

/**
 * Peer health state
 */
type PeerHealth = "unknown" | "up" | "down";

/**
 * Timeout constants
 */
const ENDPOINT_CONNECTION_TIMEOUT = 15; // seconds - expect handshake within this time
const PEER_DOWN_INTERVAL = 275; // seconds - from WireGuard whitepaper

/**
 * Parse WireGuard status output
 *
 * @param wgOutput - Output from `wg show` command
 * @returns Array of peer statuses
 */
export function parseWireGuardStatus(wgOutput: string): PeerStatus[] {
  const peers: PeerStatus[] = [];
  let currentPeer: Partial<PeerStatus> | null = null;

  const lines = wgOutput.split("\n");

  for (const line of lines) {
    const trimmed = line.trim();

    if (trimmed.startsWith("peer:")) {
      // Save previous peer
      if (currentPeer && currentPeer.publicKey) {
        peers.push(currentPeer as PeerStatus);
      }

      // Start new peer
      const pubKey = trimmed.replace("peer:", "").trim();
      currentPeer = {
        publicKey: pubKey,
        lastHandshake: Infinity, // No handshake yet
        rxBytes: 0,
        txBytes: 0,
        allowedIps: [],
      };
    } else if (currentPeer) {
      if (trimmed.startsWith("endpoint:")) {
        currentPeer.endpoint = trimmed.replace("endpoint:", "").trim();
      } else if (trimmed.startsWith("latest handshake:")) {
        const timeStr = trimmed.replace("latest handshake:", "").trim();
        currentPeer.lastHandshake = parseHandshakeTime(timeStr);
      } else if (trimmed.startsWith("transfer:")) {
        const match = trimmed.match(
          /transfer:\s+([\d.]+\s+\w+)\s+received,\s+([\d.]+\s+\w+)\s+sent/,
        );
        if (match) {
          currentPeer.rxBytes = parseBytesString(match[1]);
          currentPeer.txBytes = parseBytesString(match[2]);
        }
      } else if (trimmed.startsWith("allowed ips:")) {
        const ipsStr = trimmed.replace("allowed ips:", "").trim();
        currentPeer.allowedIps = ipsStr.split(",").map((ip) => ip.trim());
      }
    }
  }

  // Save last peer
  if (currentPeer && currentPeer.publicKey) {
    peers.push(currentPeer as PeerStatus);
  }

  return peers;
}

/**
 * Parse handshake time string to seconds
 *
 * @param timeStr - Time string like "1 minute, 30 seconds ago"
 * @returns Seconds since handshake
 */
function parseHandshakeTime(timeStr: string): number {
  if (
    timeStr.includes("never") || timeStr.includes("(never)") || !timeStr.trim()
  ) {
    return Infinity;
  }

  let seconds = 0;

  // Parse days
  const daysMatch = timeStr.match(/(\d+)\s+day/);
  if (daysMatch) {
    seconds += parseInt(daysMatch[1]) * 86400;
  }

  // Parse hours
  const hoursMatch = timeStr.match(/(\d+)\s+hour/);
  if (hoursMatch) {
    seconds += parseInt(hoursMatch[1]) * 3600;
  }

  // Parse minutes
  const minutesMatch = timeStr.match(/(\d+)\s+minute/);
  if (minutesMatch) {
    seconds += parseInt(minutesMatch[1]) * 60;
  }

  // Parse seconds
  const secondsMatch = timeStr.match(/(\d+)\s+second/);
  if (secondsMatch) {
    seconds += parseInt(secondsMatch[1]);
  }

  return seconds;
}

/**
 * Parse bytes string to number
 *
 * @param bytesStr - String like "1.23 KiB" or "456 B"
 * @returns Number of bytes
 */
function parseBytesString(bytesStr: string): number {
  const match = bytesStr.match(/([\d.]+)\s+(\w+)/);
  if (!match) return 0;

  const value = parseFloat(match[1]);
  const unit = match[2].toLowerCase();

  const multipliers: Record<string, number> = {
    "b": 1,
    "kib": 1024,
    "mib": 1024 * 1024,
    "gib": 1024 * 1024 * 1024,
    "tib": 1024 * 1024 * 1024 * 1024,
  };

  return value * (multipliers[unit] || 1);
}

/**
 * Determine peer health based on handshake time
 *
 * @param lastHandshake - Seconds since last handshake
 * @param endpointChangedRecently - Whether endpoint was changed recently
 * @returns Peer health status
 */
export function determinePeerHealth(
  lastHandshake: number,
  endpointChangedRecently: boolean,
): PeerHealth {
  if (lastHandshake === Infinity) {
    return endpointChangedRecently ? "unknown" : "down";
  }

  if (lastHandshake < ENDPOINT_CONNECTION_TIMEOUT) {
    return "up";
  }

  if (lastHandshake < PEER_DOWN_INTERVAL) {
    return endpointChangedRecently ? "unknown" : "up";
  }

  return "down";
}

/**
 * Get WireGuard peer status
 *
 * @param ssh - SSH connection to the server
 * @param interfaceName - WireGuard interface name
 * @returns Array of peer statuses
 */
export async function getWireGuardPeerStatus(
  ssh: SSHManager,
  interfaceName = "jiji0",
): Promise<PeerStatus[]> {
  const result = await ssh.executeCommand(`wg show ${interfaceName}`);

  if (result.code !== 0) {
    throw new Error(
      `Failed to get WireGuard status: ${result.stderr}`,
    );
  }

  return parseWireGuardStatus(result.stdout);
}

/**
 * Check if peer needs endpoint rotation
 *
 * @param peerStatus - Current peer status
 * @param server - Network server info with multiple endpoints
 * @returns True if rotation needed
 */
export function shouldRotateEndpoint(
  peerStatus: PeerStatus,
  server: NetworkServer,
): boolean {
  // Only rotate if peer is down
  const health = determinePeerHealth(peerStatus.lastHandshake, false);
  if (health !== "down") {
    return false;
  }

  // Only rotate if we have multiple endpoints
  if (server.endpoints.length <= 1) {
    return false;
  }

  return true;
}

/**
 * Rotate to next endpoint for a peer
 *
 * @param currentEndpoint - Current endpoint
 * @param availableEndpoints - All available endpoints
 * @returns Next endpoint to try
 */
export function getNextEndpoint(
  currentEndpoint: string | undefined,
  availableEndpoints: string[],
): string {
  if (availableEndpoints.length === 0) {
    throw new Error("No endpoints available");
  }

  if (!currentEndpoint || availableEndpoints.length === 1) {
    return availableEndpoints[0];
  }

  const currentIndex = availableEndpoints.indexOf(currentEndpoint);
  const nextIndex = (currentIndex + 1) % availableEndpoints.length;

  return availableEndpoints[nextIndex];
}

/**
 * Update WireGuard peer endpoint
 *
 * @param ssh - SSH connection
 * @param interfaceName - WireGuard interface name
 * @param publicKey - Peer's public key
 * @param newEndpoint - New endpoint to set
 */
export async function updatePeerEndpoint(
  ssh: SSHManager,
  interfaceName: string,
  publicKey: string,
  newEndpoint: string,
): Promise<void> {
  const cmd =
    `wg set ${interfaceName} peer ${publicKey} endpoint ${newEndpoint}`;
  const result = await ssh.executeCommand(cmd);

  if (result.code !== 0) {
    throw new Error(
      `Failed to update peer endpoint: ${result.stderr}`,
    );
  }

  log.info(
    `Updated peer ${publicKey.substring(0, 8)}... endpoint to ${newEndpoint}`,
    "network",
  );
}

/**
 * Monitor peers and perform health checks
 *
 * @param ssh - SSH connection to the server
 * @param servers - All network servers
 * @param localServerId - ID of the local server
 * @param interfaceName - WireGuard interface name
 */
export async function monitorPeers(
  ssh: SSHManager,
  servers: NetworkServer[],
  _localServerId: string,
  interfaceName = "jiji0",
): Promise<void> {
  try {
    // Get current peer status
    const peerStatuses = await getWireGuardPeerStatus(ssh, interfaceName);

    // Check each peer
    for (const peerStatus of peerStatuses) {
      // Find corresponding server
      const server = servers.find(
        (s) => s.wireguardPublicKey === peerStatus.publicKey,
      );

      if (!server) {
        log.warn(
          `Unknown peer ${peerStatus.publicKey.substring(0, 8)}...`,
          "network",
        );
        continue;
      }

      // Determine health
      const health = determinePeerHealth(peerStatus.lastHandshake, false);

      // Log if peer is down
      if (health === "down") {
        log.warn(
          `Peer ${server.hostname} is down (last handshake: ${
            peerStatus.lastHandshake === Infinity
              ? "never"
              : `${peerStatus.lastHandshake}s ago`
          })`,
          "network",
        );

        // Try endpoint rotation if possible
        if (shouldRotateEndpoint(peerStatus, server)) {
          const nextEndpoint = getNextEndpoint(
            peerStatus.endpoint,
            server.endpoints,
          );

          if (nextEndpoint !== peerStatus.endpoint) {
            log.info(
              `Rotating endpoint for ${server.hostname}: ${peerStatus.endpoint} -> ${nextEndpoint}`,
              "network",
            );

            await updatePeerEndpoint(
              ssh,
              interfaceName,
              peerStatus.publicKey,
              nextEndpoint,
            );
          }
        }
      }
    }
  } catch (error) {
    log.error(`Peer monitoring failed: ${error}`, "network");
  }
}

/**
 * Create systemd service for peer monitoring
 *
 * @param ssh - SSH connection to the server
 * @param servers - All network servers (for endpoint rotation)
 * @param localServerId - ID of local server
 * @param interfaceName - WireGuard interface name
 * @param checkInterval - Check interval in seconds (default: 60)
 */
export async function createPeerMonitorService(
  ssh: SSHManager,
  servers: NetworkServer[],
  localServerId: string,
  interfaceName = "jiji0",
  checkInterval = 60,
): Promise<void> {
  const host = ssh.getHost();

  // Create monitoring script
  const scriptPath = "/opt/jiji/bin/monitor-wireguard-peers.sh";

  // Build server list for the script
  const serverList = servers
    .filter((s) => s.id !== localServerId)
    .map(
      (s) => `${s.wireguardPublicKey}|${s.hostname}|${s.endpoints.join(",")}`,
    )
    .join("\n");

  const script = `#!/bin/bash
set -e

INTERFACE="${interfaceName}"
CHECK_INTERVAL=${checkInterval}
PEER_DOWN_THRESHOLD=275

# Peer definitions: publickey|hostname|endpoint1,endpoint2,...
PEERS=$(cat <<'EOF'
${serverList}
EOF
)

while true; do
  # Get WireGuard status
  WG_STATUS=$(wg show "$INTERFACE" 2>/dev/null || echo "")

  if [ -z "$WG_STATUS" ]; then
    echo "[$(date)] ERROR: Could not get WireGuard status"
    sleep $CHECK_INTERVAL
    continue
  fi

  # Parse and check each peer
  while IFS='|' read -r pubkey hostname endpoints; do
    [ -z "$pubkey" ] && continue

    # Get peer info from wg status
    PEER_INFO=$(echo "$WG_STATUS" | awk -v pk="$pubkey" '
      /^peer:/ { in_peer=0 }
      $0 ~ "peer: " pk { in_peer=1 }
      in_peer { print }
    ')

    # Extract last handshake
    HANDSHAKE=$(echo "$PEER_INFO" | grep "latest handshake:" | sed 's/.*latest handshake: //' || echo "never")

    # Convert handshake to seconds
    SECONDS_AGO=999999
    if [[ "$HANDSHAKE" =~ ([0-9]+)\ seconds?\ ago ]]; then
      SECONDS_AGO=\$\{BASH_REMATCH[1]}
    elif [[ "$HANDSHAKE" =~ ([0-9]+)\ minutes?.*([0-9]+)\ seconds?\ ago ]]; then
      SECONDS_AGO=$((\\$\{BASH_REMATCH[1]} * 60 + \\$\{BASH_REMATCH[2]}))
    fi

    # Check if peer is down
    if [ "$SECONDS_AGO" -gt "$PEER_DOWN_THRESHOLD" ]; then
      echo "[$(date)] WARNING: Peer $hostname is down (handshake: $HANDSHAKE)"

      # Try endpoint rotation if multiple endpoints available
      IFS=',' read -ra ENDPOINT_ARRAY <<< "$endpoints"
      if [ \${#ENDPOINT_ARRAY[@]} -gt 1 ]; then
        # Get current endpoint
        CURRENT_ENDPOINT=$(echo "$PEER_INFO" | grep "endpoint:" | awk '{print $2}')

        # Find next endpoint
        NEXT_ENDPOINT=""
        for i in "\${!ENDPOINT_ARRAY[@]}"; do
          if [ "\${ENDPOINT_ARRAY[$i]}" = "$CURRENT_ENDPOINT" ]; then
            NEXT_INDEX=$(( (i + 1) % \${#ENDPOINT_ARRAY[@]} ))
            NEXT_ENDPOINT="\${ENDPOINT_ARRAY[$NEXT_INDEX]}"
            break
          fi
        done

        # Use first endpoint if current not found
        [ -z "$NEXT_ENDPOINT" ] && NEXT_ENDPOINT="\${ENDPOINT_ARRAY[0]}"

        if [ "$NEXT_ENDPOINT" != "$CURRENT_ENDPOINT" ]; then
          echo "[$(date)] INFO: Rotating endpoint for $hostname: $CURRENT_ENDPOINT -> $NEXT_ENDPOINT"
          wg set "$INTERFACE" peer "$pubkey" endpoint "$NEXT_ENDPOINT"
        fi
      fi
    fi
  done <<< "$PEERS"

  sleep $CHECK_INTERVAL
done
`;

  // Create directory
  await ssh.executeCommand("mkdir -p /opt/jiji/bin");

  // Write script
  const writeResult = await ssh.executeCommand(
    `cat > ${scriptPath} << 'EOFSCRIPT'\n${script}\nEOFSCRIPT`,
  );

  if (writeResult.code !== 0) {
    throw new Error(`Failed to write monitoring script: ${writeResult.stderr}`);
  }

  // Make executable
  await ssh.executeCommand(`chmod +x ${scriptPath}`);

  // Create systemd service
  const serviceContent = `[Unit]
Description=Jiji WireGuard Peer Monitor
After=wg-quick@${interfaceName}.service
Requires=wg-quick@${interfaceName}.service

[Service]
Type=simple
ExecStart=${scriptPath}
Restart=always
RestartSec=10

[Install]
WantedBy=multi-user.target
`;

  const serviceResult = await ssh.executeCommand(
    `cat > /etc/systemd/system/jiji-peer-monitor.service << 'EOFSERVICE'\n${serviceContent}\nEOFSERVICE`,
  );

  if (serviceResult.code !== 0) {
    throw new Error(
      `Failed to create peer monitor service: ${serviceResult.stderr}`,
    );
  }

  // Reload systemd
  await ssh.executeCommand("systemctl daemon-reload");

  // Enable and start service
  await ssh.executeCommand("systemctl enable jiji-peer-monitor.service");
  await ssh.executeCommand("systemctl restart jiji-peer-monitor.service");

  log.success(`Peer monitoring service created on ${host}`, "network");
}

/**
 * Stop and remove peer monitoring service
 *
 * @param ssh - SSH connection to the server
 */
export async function removePeerMonitorService(
  ssh: SSHManager,
): Promise<void> {
  await ssh.executeCommand(
    "systemctl stop jiji-peer-monitor.service 2>/dev/null || true",
  );
  await ssh.executeCommand(
    "systemctl disable jiji-peer-monitor.service 2>/dev/null || true",
  );
  await ssh.executeCommand(
    "rm -f /etc/systemd/system/jiji-peer-monitor.service",
  );
  await ssh.executeCommand("systemctl daemon-reload");

  log.debug("Peer monitoring service removed", "network");
}
