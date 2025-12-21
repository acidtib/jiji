/**
 * Unified WireGuard control loop for network reconciliation
 *
 * This control loop runs continuously on each server and handles:
 * 1. Topology reconciliation - Add/remove WireGuard peers based on Corrosion state
 * 2. Endpoint health monitoring - Check handshake times and rotate endpoints
 * 3. Container health tracking - Validate containers and update Corrosion
 * 4. Heartbeat updates - Keep server alive in Corrosion
 * 5. Public IP discovery - Periodically refresh endpoints
 */

import type { SSHManager } from "../../utils/ssh.ts";
import { log } from "../../utils/logger.ts";

const LOOP_INTERVAL = 30; // seconds
const PEER_DOWN_THRESHOLD = 275; // seconds - from WireGuard spec
const ENDPOINT_CONNECTION_TIMEOUT = 15; // seconds

/**
 * Generate the unified control loop bash script
 *
 * @param serverId - ID of the local server
 * @param engine - Container engine (docker or podman)
 * @param interfaceName - WireGuard interface name (default: jiji0)
 * @returns Bash script content
 */
export function generateControlLoopScript(
  serverId: string,
  engine: "docker" | "podman",
  interfaceName: string = "jiji0",
): string {
  return `#!/bin/bash
set -euo pipefail

# Jiji Unified Control Loop
# Runs every ${LOOP_INTERVAL} seconds to maintain network health

SERVER_ID="${serverId}"
ENGINE="${engine}"
INTERFACE="${interfaceName}"
LOOP_INTERVAL=${LOOP_INTERVAL}
PEER_DOWN_THRESHOLD=${PEER_DOWN_THRESHOLD}
ENDPOINT_TIMEOUT=${ENDPOINT_CONNECTION_TIMEOUT}
ITERATION=0
CORROSION_DIR="/opt/jiji/corrosion"

log_info() {
  echo "[$(date '+%Y-%m-%d %H:%M:%S')] INFO: $*"
}

log_warn() {
  echo "[$(date '+%Y-%m-%d %H:%M:%S')] WARN: $*"
}

log_error() {
  echo "[$(date '+%Y-%m-%d %H:%M:%S')] ERROR: $*"
}

# Parse handshake time from wg show output
parse_handshake() {
  local handshake="$1"
  local seconds=999999

  if [[ "$handshake" == *"never"* ]] || [[ -z "$handshake" ]]; then
    echo "$seconds"
    return
  fi

  # Parse "X days, Y hours, Z minutes, W seconds ago" format
  if [[ "$handshake" =~ ([0-9]+)\\ day ]]; then
    seconds=$((seconds + \${BASH_REMATCH[1]} * 86400))
  fi
  if [[ "$handshake" =~ ([0-9]+)\\ hour ]]; then
    seconds=$((seconds + \${BASH_REMATCH[1]} * 3600))
  fi
  if [[ "$handshake" =~ ([0-9]+)\\ minute ]]; then
    seconds=$((seconds + \${BASH_REMATCH[1]} * 60))
  fi
  if [[ "$handshake" =~ ([0-9]+)\\ second ]]; then
    seconds=$((seconds + \${BASH_REMATCH[1]}))
  fi

  # If no match, assume recent (0 seconds)
  if [ $seconds -eq 999999 ]; then
    seconds=0
  fi

  echo "$seconds"
}

# Update heartbeat in Corrosion
update_heartbeat() {
  local now=$(date +%s)000  # milliseconds
  local sql="UPDATE servers SET last_seen = $now WHERE id = '$SERVER_ID';"

  \${CORROSION_DIR}/corrosion exec --config \${CORROSION_DIR}/config.toml "$sql" 2>/dev/null || log_error "Failed to update heartbeat"
}

# Reconcile WireGuard peers with Corrosion state
reconcile_peers() {
  log_info "Reconciling WireGuard peers..."

  # Get active servers from Corrosion (seen in last 5 minutes)
  local active_servers=$(\${CORROSION_DIR}/corrosion query --config \${CORROSION_DIR}/config.toml "SELECT wireguard_pubkey, subnet, management_ip, endpoints FROM servers WHERE last_seen > (strftime('%s', 'now') - 300) * 1000 AND id != '$SERVER_ID';" 2>/dev/null || echo "")

  if [ -z "$active_servers" ]; then
    log_warn "No active servers found in Corrosion"
    return
  fi

  # Get current WireGuard peers
  local current_peers=$(wg show $INTERFACE dump 2>/dev/null | tail -n +2 | awk '{print $1}')

  # Add missing peers
  while IFS='|' read -r pubkey subnet mgmt_ip endpoints; do
    [ -z "$pubkey" ] && continue

    if ! echo "$current_peers" | grep -q "$pubkey"; then
      log_info "Adding new peer: $pubkey"

      # Parse first endpoint from JSON array
      local endpoint=$(echo "$endpoints" | sed 's/\\[//; s/\\]//; s/"//g' | cut -d',' -f1 | tr -d ' ')

      # Add peer
      wg set $INTERFACE peer "$pubkey" \
        allowed-ips "$subnet,$mgmt_ip/128" \
        endpoint "$endpoint" \
        persistent-keepalive 25 2>/dev/null || log_error "Failed to add peer $pubkey"
    fi
  done <<< "$active_servers"

  # Remove stale peers (not in active servers list)
  for peer in $current_peers; do
    if ! echo "$active_servers" | grep -q "$peer"; then
      log_warn "Removing stale peer: $peer"
      wg set $INTERFACE peer "$peer" remove 2>/dev/null || log_error "Failed to remove peer $peer"
    fi
  done
}

# Monitor peer health and rotate endpoints
monitor_peer_health() {
  local wg_status=$(wg show $INTERFACE dump 2>/dev/null | tail -n +2)

  while read -r line; do
    [ -z "$line" ] && continue

    local pubkey=$(echo "$line" | awk '{print $1}')
    local endpoint=$(echo "$line" | awk '{print $3}')
    local latest_handshake=$(echo "$line" | awk '{print $5}')

    # Calculate seconds since last handshake
    local now=$(date +%s)
    local handshake_age=$((now - latest_handshake))

    if [ $handshake_age -gt $PEER_DOWN_THRESHOLD ]; then
      log_warn "Peer $pubkey is down (handshake: \${handshake_age}s ago)"

      # Try to rotate endpoint
      local endpoints=$(\${CORROSION_DIR}/corrosion query --config \${CORROSION_DIR}/config.toml "SELECT endpoints FROM servers WHERE wireguard_pubkey = '$pubkey';" 2>/dev/null || echo "")

      if [ -n "$endpoints" ]; then
        # Parse all endpoints from JSON array
        local all_endpoints=$(echo "$endpoints" | sed 's/\\[//; s/\\]//; s/"//g' | tr ',' '\\n')
        local endpoint_count=$(echo "$all_endpoints" | wc -l)

        if [ $endpoint_count -gt 1 ]; then
          # Find current endpoint index and rotate to next
          local current_index=0
          local i=0
          while IFS= read -r ep; do
            if [ "$ep" == "$endpoint" ]; then
              current_index=$i
              break
            fi
            i=$((i + 1))
          done <<< "$all_endpoints"

          local next_index=$(( (current_index + 1) % endpoint_count ))
          local next_endpoint=$(echo "$all_endpoints" | sed -n "$((next_index + 1))p")

          if [ "$next_endpoint" != "$endpoint" ]; then
            log_info "Rotating endpoint for $pubkey: $endpoint -> $next_endpoint"
            wg set $INTERFACE peer "$pubkey" endpoint "$next_endpoint" 2>/dev/null || log_error "Failed to update endpoint"
          fi
        fi
      fi
    elif [ $handshake_age -lt $ENDPOINT_TIMEOUT ]; then
      # Peer is healthy - could persist successful endpoint here if needed
      :
    fi
  done <<< "$wg_status"
}

# Sync container health states
sync_container_health() {
  local changes=0

  # Get containers registered on this server
  local registered=$(\${CORROSION_DIR}/corrosion query --config \${CORROSION_DIR}/config.toml "SELECT id FROM containers WHERE server_id = '$SERVER_ID';" 2>/dev/null || echo "")

  while IFS= read -r container_id; do
    [ -z "$container_id" ] && continue

    # Check if container is running
    if $ENGINE ps -q --filter name=$container_id 2>/dev/null | grep -q .; then
      # Container is running, mark healthy
      \${CORROSION_DIR}/corrosion exec --config \${CORROSION_DIR}/config.toml "UPDATE containers SET healthy = 1 WHERE id = '$container_id';" 2>/dev/null || true
    else
      # Container stopped or removed, mark unhealthy
      \${CORROSION_DIR}/corrosion exec --config \${CORROSION_DIR}/config.toml "UPDATE containers SET healthy = 0 WHERE id = '$container_id';" 2>/dev/null || true
      changes=$((changes + 1))
    fi
  done <<< "$registered"

  # Trigger DNS update if any changes
  if [ $changes -gt 0 ]; then
    log_info "Container health changed, triggering DNS update"
    systemctl restart jiji-dns-update.service 2>/dev/null || true
  fi
}

# Discover and update public IP (every 10 minutes)
update_public_ip() {
  # Only run every 20 iterations (20 * 30s = 10 minutes)
  if [ $((ITERATION % 20)) -ne 0 ]; then
    return
  fi

  log_info "Checking for public IP changes..."

  # Try multiple services
  local new_ip=""
  for service in "https://api.ipify.org" "https://ipinfo.io/ip" "https://icanhazip.com"; do
    new_ip=$(curl -s --max-time 5 "$service" 2>/dev/null || echo "")

    # Validate IP format
    if [[ "$new_ip" =~ ^[0-9]+\\.[0-9]+\\.[0-9]+\\.[0-9]+$ ]]; then
      break
    else
      new_ip=""
    fi
  done

  if [ -z "$new_ip" ]; then
    log_warn "Could not discover public IP"
    return
  fi

  # Get current endpoints
  local current=$(\${CORROSION_DIR}/corrosion query --config \${CORROSION_DIR}/config.toml "SELECT endpoints FROM servers WHERE id = '$SERVER_ID';" 2>/dev/null || echo "")

  # Check if public IP changed
  if ! echo "$current" | grep -q "$new_ip"; then
    log_info "Public IP changed to $new_ip, updating endpoints"

    # Update endpoints in Corrosion (this is simplified - real implementation would merge with existing)
    local new_endpoint="$new_ip:51820"
    local endpoints_json="[\\"$new_endpoint\\"]"
    $CORROSION_DIR/corrosion exec --config $CORROSION_DIR/config.toml "UPDATE servers SET endpoints = '$endpoints_json' WHERE id = '$SERVER_ID';" 2>/dev/null || log_error "Failed to update endpoints"
  fi
}

# Main loop
log_info "Starting Jiji control loop for server $SERVER_ID"
log_info "Loop interval: ${LOOP_INTERVAL}s, Interface: $INTERFACE"

while true; do
  ITERATION=$((ITERATION + 1))

  # 1. Update heartbeat
  update_heartbeat

  # 2. Reconcile WireGuard peers
  reconcile_peers

  # 3. Monitor peer health
  monitor_peer_health

  # 4. Sync container health
  sync_container_health

  # 5. Update public IP (periodic)
  update_public_ip

  # Sleep before next iteration
  sleep $LOOP_INTERVAL
done
`;
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
  const host = ssh.getHost();

  log.info(`Creating control loop service on ${host}`, "network");

  // Generate bash script
  const script = generateControlLoopScript(serverId, engine, interfaceName);
  const scriptPath = "/opt/jiji/bin/jiji-control-loop.sh";

  // Create directory
  await ssh.executeCommand("mkdir -p /opt/jiji/bin");

  // Write script
  const writeResult = await ssh.executeCommand(
    `cat > ${scriptPath} << 'EOFSCRIPT'\n${script}\nEOFSCRIPT`,
  );

  if (writeResult.code !== 0) {
    throw new Error(
      `Failed to write control loop script: ${writeResult.stderr}`,
    );
  }

  // Make executable
  await ssh.executeCommand(`chmod +x ${scriptPath}`);

  // Create systemd service
  const serviceContent = `[Unit]
Description=Jiji Network Control Loop
After=jiji-corrosion.service
Requires=jiji-corrosion.service

[Service]
Type=simple
ExecStart=${scriptPath}
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

  log.success(`Control loop service created and started on ${host}`, "network");
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
  await ssh.executeCommand("rm -f /opt/jiji/bin/jiji-control-loop.sh");
  await ssh.executeCommand("systemctl daemon-reload");

  log.debug("Control loop service removed", "network");
}
