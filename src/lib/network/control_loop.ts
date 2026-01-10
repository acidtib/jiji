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
SHUTTING_DOWN=0

# Cleanup function for graceful shutdown
cleanup() {
  log_info "Received shutdown signal, cleaning up..."
  log_info_json "Shutdown initiated" "{}"
  SHUTTING_DOWN=1

  # Update heartbeat one last time to signal we're going offline
  update_heartbeat 2>/dev/null || true

  log_info "Control loop shutdown complete"
  exit 0
}

# Error handler - logs but doesn't exit to keep loop running
handle_error() {
  local line_no=\$1
  local error_code=\$2
  log_error "Error on line \$line_no (exit code: \$error_code)"
  log_error_json "Control loop error" "{\\"line\\":\$line_no,\\"code\\":\$error_code}"
  # Don't exit - continue with next iteration
}

# Set up signal handlers
trap cleanup SIGTERM SIGINT SIGHUP
trap 'handle_error \$LINENO \$?' ERR

log_info() {
  echo "[$(date '+%Y-%m-%d %H:%M:%S')] INFO: $*"
}

log_warn() {
  echo "[$(date '+%Y-%m-%d %H:%M:%S')] WARN: $*"
}

log_error() {
  echo "[$(date '+%Y-%m-%d %H:%M:%S')] ERROR: $*"
}

# JSON logging for machine-parseable output
# Set JIJI_JSON_LOGS=1 to enable JSON-only output
log_json() {
  local level="$1"
  local message="$2"
  local data="\${3:-}"

  local timestamp=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

  if [ -n "$data" ]; then
    echo "{\\"timestamp\\":\\"$timestamp\\",\\"level\\":\\"$level\\",\\"server_id\\":\\"$SERVER_ID\\",\\"message\\":\\"$message\\",\\"data\\":$data}"
  else
    echo "{\\"timestamp\\":\\"$timestamp\\",\\"level\\":\\"$level\\",\\"server_id\\":\\"$SERVER_ID\\",\\"message\\":\\"$message\\"}"
  fi
}

log_info_json() {
  log_json "info" "$1" "\${2:-}"
}

log_warn_json() {
  log_json "warn" "$1" "\${2:-}"
}

log_error_json() {
  log_json "error" "$1" "\${2:-}"
}

# Execute SQL via Corrosion HTTP API
# Using HTTP API ensures subscription events are triggered for real-time DNS updates
corrosion_exec() {
  local sql="$1"
  # Escape double quotes in SQL for JSON, then POST to Corrosion API
  curl -sf -X POST -H "Content-Type: application/json" \\
    -d "[\\"$(echo "$sql" | sed 's/"/\\\\"/g')\\"]" \\
    http://127.0.0.1:9220/v1/transactions
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

  corrosion_exec "$sql" 2>/dev/null || log_error "Failed to update heartbeat"
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

# Perform TCP health check on container port
# Returns 0 if healthy, 1 if unhealthy
check_container_tcp_health() {
  local container_ip="\$1"
  local port="\$2"

  if [ -z "\$port" ] || [ "\$port" = "null" ] || [ "\$port" = "0" ]; then
    # No port specified, fall back to process check only (assume healthy if running)
    return 0
  fi

  # Use timeout with bash TCP check (faster than nc and more portable)
  if timeout 2 bash -c "echo > /dev/tcp/\$container_ip/\$port" 2>/dev/null; then
    return 0
  else
    return 1
  fi
}

# Sync container health states with granular health tracking
# Supports TCP health checks when health_port is configured
sync_container_health() {
  local changes=0

  # Get containers with health info from this server
  # Format: id|ip|health_port|health_status|consecutive_failures
  local registered=$(\${CORROSION_DIR}/corrosion query --config \${CORROSION_DIR}/config.toml "
    SELECT id, ip, health_port, health_status, consecutive_failures
    FROM containers
    WHERE server_id = '$SERVER_ID';" 2>/dev/null || echo "")

  while IFS='|' read -r container_id container_ip health_port current_status consecutive_failures; do
    [ -z "\$container_id" ] && continue

    local new_status=""
    local new_failures=\${consecutive_failures:-0}
    local now=$(date +%s)000

    # Check if container process is running
    if ! $ENGINE ps -q --filter id=\$container_id 2>/dev/null | grep -q .; then
      # Container not running - definitely unhealthy
      new_status="unhealthy"
      new_failures=\$((new_failures + 1))
    else
      # Container is running, perform TCP health check if port configured
      if check_container_tcp_health "\$container_ip" "\$health_port"; then
        # Health check passed
        new_status="healthy"
        new_failures=0
      else
        # Health check failed
        new_failures=\$((new_failures + 1))
        if [ \$new_failures -ge 3 ]; then
          new_status="unhealthy"
        elif [ \$new_failures -ge 1 ]; then
          new_status="degraded"
        fi
      fi
    fi

    # Update database if status or failures changed
    if [ "\$new_status" != "\$current_status" ] || [ \$new_failures -ne \${consecutive_failures:-0} ]; then
      corrosion_exec "UPDATE containers SET health_status = '\$new_status', last_health_check = \$now, consecutive_failures = \$new_failures WHERE id = '\$container_id';" 2>/dev/null || true

      if [ "\$new_status" != "\$current_status" ]; then
        log_info "Container \$container_id health: \$current_status -> \$new_status"
        log_info_json "Container health changed" "{\\"container_id\\":\\"\$container_id\\",\\"from\\":\\"\$current_status\\",\\"to\\":\\"\$new_status\\",\\"failures\\":\$new_failures}"
        changes=\$((changes + 1))
      fi
    fi
  done <<< "\$registered"

  # Log health changes (DNS updates happen automatically via Corrosion HTTP API subscriptions)
  if [ \$changes -gt 0 ]; then
    log_info "Container health changed (\$changes updates via HTTP API)"
  fi
}

# Cluster-wide garbage collection (runs every 5 minutes)
# Removes stale container records that have been unhealthy for too long
garbage_collect_containers() {
  # Only run every 10 iterations (10 * 30s = 5 minutes)
  if [ $((ITERATION % 10)) -ne 0 ]; then
    return
  fi

  log_info "Running cluster-wide container garbage collection..."

  local now=$(date +%s)
  local stale_threshold=$((now - 180))  # 3 minutes in seconds
  local deleted=0

  # Find containers that have been unhealthy for more than 3 minutes
  # We convert started_at from milliseconds to seconds for comparison
  local stale_containers=$(\${CORROSION_DIR}/corrosion query --config \${CORROSION_DIR}/config.toml "
    SELECT id, service
    FROM containers
    WHERE health_status != 'healthy'
    AND (started_at / 1000) < $stale_threshold
  " 2>/dev/null || echo "")

  while IFS='|' read -r container_id service; do
    [ -z "$container_id" ] && continue

    log_info "Deleting stale container: $container_id (service: $service)"
    corrosion_exec "DELETE FROM containers WHERE id = '$container_id';" 2>/dev/null || log_error "Failed to delete container $container_id"
    deleted=$((deleted + 1))
  done <<< "$stale_containers"

  # Also delete containers from servers that are offline (no heartbeat in 10 minutes)
  local offline_threshold=$((now - 600))000  # 10 minutes in milliseconds
  local offline_servers=$(\${CORROSION_DIR}/corrosion query --config \${CORROSION_DIR}/config.toml "
    SELECT id
    FROM servers
    WHERE last_seen < $offline_threshold
    AND id != '$SERVER_ID'
  " 2>/dev/null || echo "")

  while IFS= read -r server_id; do
    [ -z "$server_id" ] && continue

    log_warn "Server $server_id appears offline, cleaning up its containers"
    # Parse rows_affected from JSON response: {"results":[{"rows_affected":N,...}],...}
    local result=$(corrosion_exec "DELETE FROM containers WHERE server_id = '$server_id';" 2>/dev/null || echo "{}")
    local count=$(echo "$result" | grep -oP '"rows_affected":\\K[0-9]+' || echo "0")
    deleted=$((deleted + count))
  done <<< "$offline_servers"

  if [ $deleted -gt 0 ]; then
    log_info "Garbage collection complete: removed $deleted stale container record(s) (DNS updates via HTTP API subscriptions)"
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
    corrosion_exec "UPDATE servers SET endpoints = '$endpoints_json' WHERE id = '$SERVER_ID';" 2>/dev/null || log_error "Failed to update endpoints"
  fi
}

# Check Corrosion service health (every 10 minutes)
check_corrosion_health() {
  # Only run every 20 iterations (20 * 30s = 10 minutes)
  if [ $((ITERATION % 20)) -ne 0 ]; then
    return
  fi

  log_info "Checking Corrosion health..."

  # Check if Corrosion service is running
  if ! systemctl is-active --quiet jiji-corrosion; then
    log_error "Corrosion service is not running!"
    log_error_json "Corrosion service not running" "{\\"action\\":\\"restart_attempted\\"}"
    # Attempt restart
    if systemctl restart jiji-corrosion 2>/dev/null; then
      log_info "Corrosion service restarted successfully"
      log_info_json "Corrosion service restarted" "{}"
      sleep 5  # Give it time to start
    else
      log_error "Failed to restart Corrosion service"
      log_error_json "Corrosion restart failed" "{}"
      return
    fi
  fi

  # Test database connectivity with simple query
  local test_result=$(\${CORROSION_DIR}/corrosion query --config \${CORROSION_DIR}/config.toml "SELECT 1;" 2>&1)
  if [ $? -ne 0 ]; then
    log_error "Corrosion database query failed: $test_result"
    log_error_json "Corrosion query failed" "{\\"error\\":\\"$test_result\\"}"
    return
  fi

  # Check if heartbeat is being recorded (self-check)
  local last_seen=$(\${CORROSION_DIR}/corrosion query --config \${CORROSION_DIR}/config.toml "SELECT last_seen FROM servers WHERE id = '$SERVER_ID';" 2>/dev/null || echo "0")
  local now=$(date +%s)000
  local age=$((now - last_seen))

  # If heartbeat is older than 2 minutes, something is wrong
  if [ $age -gt 120000 ]; then
    log_warn "Heartbeat appears stale (age: \${age}ms)"
    log_warn_json "Heartbeat stale" "{\\"age_ms\\":$age}"
  fi

  log_info "Corrosion health check passed"
}

# Detect cluster partition / split-brain scenario
detect_split_brain() {
  # Only run every 20 iterations (20 * 30s = 10 minutes)
  if [ $((ITERATION % 20)) -ne 0 ]; then
    return
  fi

  log_info "Checking for cluster partition..."

  # Get total number of registered servers
  local total_servers=$(\${CORROSION_DIR}/corrosion query --config \${CORROSION_DIR}/config.toml "SELECT COUNT(*) FROM servers;" 2>/dev/null || echo "0")

  if [ "$total_servers" = "0" ] || [ -z "$total_servers" ]; then
    log_warn "Cannot determine total server count"
    return
  fi

  # Get number of servers with recent heartbeat (active in last 5 minutes)
  local now=$(date +%s)000
  local active_threshold=$((now - 300000))  # 5 minutes in milliseconds
  local active_servers=$(\${CORROSION_DIR}/corrosion query --config \${CORROSION_DIR}/config.toml "SELECT COUNT(*) FROM servers WHERE last_seen > $active_threshold;" 2>/dev/null || echo "0")

  # Calculate reachability percentage
  local reachable_pct=0
  if [ "$total_servers" -gt 0 ]; then
    reachable_pct=$((active_servers * 100 / total_servers))
  fi

  log_info "Cluster health: $active_servers/$total_servers servers reachable ($reachable_pct%)"
  log_info_json "Cluster health check" "{\\"active\\":$active_servers,\\"total\\":$total_servers,\\"percent\\":$reachable_pct}"

  # Alert if less than 50% of cluster is reachable (and we have more than 1 server)
  if [ $total_servers -gt 1 ] && [ $reachable_pct -lt 50 ]; then
    log_error "POTENTIAL SPLIT-BRAIN: Only $reachable_pct% of cluster reachable ($active_servers/$total_servers servers)"
    log_error_json "Split-brain detected" "{\\"active\\":$active_servers,\\"total\\":$total_servers,\\"percent\\":$reachable_pct}"

    # Log unreachable servers for debugging
    local unreachable=$(\${CORROSION_DIR}/corrosion query --config \${CORROSION_DIR}/config.toml "SELECT hostname FROM servers WHERE last_seen <= $active_threshold;" 2>/dev/null || echo "")
    if [ -n "$unreachable" ]; then
      log_error "Unreachable servers: $unreachable"
    fi
  fi
}

# Main loop
log_info "Starting Jiji control loop for server $SERVER_ID"
log_info "Loop interval: ${LOOP_INTERVAL}s, Interface: $INTERFACE"
log_info_json "Control loop started" "{\\"server_id\\":\\"$SERVER_ID\\",\\"interval\\":$LOOP_INTERVAL}"

while true; do
  # Check if we should exit
  [ \$SHUTTING_DOWN -eq 1 ] && break

  ITERATION=\$((ITERATION + 1))
  ITERATION_START=\$(date +%s)

  # 1. Update heartbeat
  update_heartbeat

  # 2. Reconcile WireGuard peers
  reconcile_peers

  # 3. Monitor peer health
  monitor_peer_health

  # 4. Sync container health (local containers with TCP checks)
  sync_container_health

  # 5. Cluster-wide garbage collection (periodic)
  garbage_collect_containers

  # 6. Update public IP (periodic)
  update_public_ip

  # 7. Check Corrosion health (periodic)
  check_corrosion_health

  # 8. Detect cluster partition / split-brain (periodic)
  detect_split_brain

  # Check iteration timing - warn if slow
  ITERATION_END=\$(date +%s)
  ITERATION_DURATION=\$((ITERATION_END - ITERATION_START))

  if [ \$ITERATION_DURATION -gt 15 ]; then
    log_warn "Slow iteration #\$ITERATION: \${ITERATION_DURATION}s (threshold: 15s)"
    log_warn_json "Slow iteration" "{\\"iteration\\":\$ITERATION,\\"duration_s\\":\$ITERATION_DURATION}"
  fi

  # Log milestone every 100 iterations (50 minutes)
  if [ \$((ITERATION % 100)) -eq 0 ]; then
    log_info "Completed \$ITERATION iterations"
    log_info_json "Iteration milestone" "{\\"iteration\\":\$ITERATION}"
  fi

  # Sleep before next iteration
  sleep \$LOOP_INTERVAL
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
