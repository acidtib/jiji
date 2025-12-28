/**
 * Network routing configuration for cross-machine container communication
 *
 * This module handles the routing setup required for containers to communicate
 * across machines via the WireGuard mesh network.
 */

import type { SSHManager } from "../../utils/ssh.ts";
import { log } from "../../utils/logger.ts";

/**
 * Configure IP forwarding on the system
 *
 * @param ssh - SSH connection to the server
 */
export async function enableIPForwarding(ssh: SSHManager): Promise<void> {
  const host = ssh.getHost();

  // Enable IPv4 forwarding
  const result = await ssh.executeCommand("sysctl -w net.ipv4.ip_forward=1");
  if (result.code !== 0) {
    throw new Error(
      `Failed to enable IP forwarding on ${host}: ${result.stderr}`,
    );
  }

  // Make it persistent
  const persistResult = await ssh.executeCommand(
    `grep -q '^net.ipv4.ip_forward=1' /etc/sysctl.conf || echo 'net.ipv4.ip_forward=1' >> /etc/sysctl.conf`,
  );

  if (persistResult.code !== 0) {
    log.warn(`Could not persist IP forwarding on ${host}`, "network");
  }

  log.debug(`IP forwarding enabled on ${host}`, "network");
}

/**
 * Add routes to peer subnets via WireGuard interface
 *
 * @param ssh - SSH connection to the server
 * @param peers - Array of peer subnet information
 * @param wireguardInterface - WireGuard interface name (default: jiji0)
 */
export async function addPeerRoutes(
  ssh: SSHManager,
  peers: Array<{ subnet: string; hostname: string }>,
  wireguardInterface = "jiji0",
): Promise<void> {
  const host = ssh.getHost();

  for (const peer of peers) {
    // Add route to peer's container subnet
    const routeCmd =
      `ip route add ${peer.subnet} dev ${wireguardInterface} 2>/dev/null || ip route replace ${peer.subnet} dev ${wireguardInterface}`;
    const result = await ssh.executeCommand(routeCmd);

    if (result.code !== 0 && !result.stderr.includes("File exists")) {
      log.warn(
        `Failed to add route to ${peer.subnet} on ${host}: ${result.stderr}`,
        "network",
      );
    } else {
      log.debug(
        `Added route to ${peer.hostname} (${peer.subnet}) via ${wireguardInterface} on ${host}`,
        "network",
      );
    }
  }

  log.debug(
    `Configured ${peers.length} peer routes on ${host}`,
    "network",
  );
}

/**
 * Remove routes to peer subnets
 *
 * @param ssh - SSH connection to the server
 * @param subnets - Array of subnet CIDRs to remove
 */
export async function removePeerRoutes(
  ssh: SSHManager,
  subnets: string[],
): Promise<void> {
  const host = ssh.getHost();

  for (const subnet of subnets) {
    const result = await ssh.executeCommand(
      `ip route del ${subnet} 2>/dev/null || true`,
    );

    if (result.code === 0) {
      log.debug(`Removed route to ${subnet} on ${host}`, "network");
    }
  }
}

/**
 * Get Docker bridge interface name
 *
 * Dynamically discovers the bridge interface by querying the network's
 * gateway IP and finding which interface has that IP assigned. This works
 * for both Docker and Podman regardless of network backend (CNI/Netavark).
 *
 * @param ssh - SSH connection to the server
 * @param networkName - Docker network name
 * @param engine - Container engine (docker or podman)
 * @returns Bridge interface name (e.g., "br-abc123", "podman1", or "docker0")
 */
export async function getDockerBridgeInterface(
  ssh: SSHManager,
  networkName: string,
  engine: "docker" | "podman",
): Promise<string> {
  const host = ssh.getHost();

  // Get network gateway IP (this is the IP assigned to the bridge interface)
  const inspectCmd =
    `${engine} network inspect ${networkName} --format '{{range .Subnets}}{{.Gateway}}{{end}}'`;
  const inspectResult = await ssh.executeCommand(inspectCmd);

  if (inspectResult.code !== 0) {
    throw new Error(
      `Failed to inspect network ${networkName} on ${host}: ${inspectResult.stderr}`,
    );
  }

  const gatewayIp = inspectResult.stdout.trim();

  if (!gatewayIp) {
    throw new Error(
      `Could not determine gateway IP for network ${networkName} on ${host}`,
    );
  }

  log.debug(
    `Network ${networkName} has gateway IP ${gatewayIp}`,
    "network",
  );

  // Find which interface has this gateway IP assigned
  const findBridgeCmd =
    `ip addr show | grep -B 2 "inet ${gatewayIp}" | head -n 1 | awk '{print $2}' | sed 's/:$//'`;
  const bridgeResult = await ssh.executeCommand(findBridgeCmd);

  if (bridgeResult.code === 0 && bridgeResult.stdout.trim()) {
    const bridgeName = bridgeResult.stdout.trim();
    log.debug(
      `Found bridge interface ${bridgeName} for network ${networkName}`,
      "network",
    );
    return bridgeName;
  }

  // Fallback to docker0 if we can't find the bridge
  log.warn(
    `Could not find bridge interface for network ${networkName} on ${host}, using docker0`,
    "network",
  );
  return "docker0";
}

/**
 * Configure iptables rules for container-to-container communication
 *
 * @param ssh - SSH connection to the server
 * @param localSubnet - Local WireGuard subnet CIDR
 * @param containerSubnet - Local container subnet CIDR
 * @param clusterCidr - Cluster-wide CIDR for all containers
 * @param dockerBridge - Docker bridge interface name
 * @param wireguardInterface - WireGuard interface name (default: jiji0)
 */
export async function setupIPTablesRules(
  ssh: SSHManager,
  localSubnet: string,
  containerSubnet: string,
  clusterCidr: string,
  dockerBridge: string,
  wireguardInterface = "jiji0",
): Promise<void> {
  const host = ssh.getHost();

  // Install iptables-persistent if not available
  await ssh.executeCommand(
    "command -v iptables-save >/dev/null 2>&1 || DEBIAN_FRONTEND=noninteractive apt-get install -y -qq iptables-persistent 2>/dev/null || true",
  );

  // Allow forwarding from Docker to WireGuard
  const forwardDockerToWg =
    `iptables -C FORWARD -i ${dockerBridge} -o ${wireguardInterface} -j ACCEPT 2>/dev/null || iptables -A FORWARD -i ${dockerBridge} -o ${wireguardInterface} -j ACCEPT`;
  const result1 = await ssh.executeCommand(forwardDockerToWg);
  if (result1.code !== 0 && !result1.stderr.includes("Bad rule")) {
    log.warn(
      `Failed to add forward rule (docker->wg) on ${host}`,
      "network",
    );
  }

  // Allow forwarding from WireGuard to Docker
  const forwardWgToDocker =
    `iptables -C FORWARD -i ${wireguardInterface} -o ${dockerBridge} -j ACCEPT 2>/dev/null || iptables -A FORWARD -i ${wireguardInterface} -o ${dockerBridge} -j ACCEPT`;
  const result2 = await ssh.executeCommand(forwardWgToDocker);
  if (result2.code !== 0 && !result2.stderr.includes("Bad rule")) {
    log.warn(
      `Failed to add forward rule (wg->docker) on ${host}`,
      "network",
    );
  }

  // Skip NAT for WireGuard-to-cluster traffic
  // This preserves source IPs for WireGuard subnet traffic within the cluster
  const skipNatWireGuardRule =
    `iptables -t nat -C POSTROUTING -s ${localSubnet} -d ${clusterCidr} -j RETURN 2>/dev/null || iptables -t nat -A POSTROUTING -s ${localSubnet} -d ${clusterCidr} -j RETURN`;
  const result3 = await ssh.executeCommand(skipNatWireGuardRule);
  if (result3.code !== 0 && !result3.stderr.includes("Bad rule")) {
    log.warn(
      `Failed to add skip NAT rule for WireGuard traffic on ${host}`,
      "network",
    );
  }

  // Skip NAT for container-to-cluster traffic
  // This is critical - prevents container source IPs from being masqueraded to host IP
  // Without this, containers can't communicate across servers (handshake failures)
  // IMPORTANT: Must INSERT (not APPEND) to ensure this rule comes BEFORE any MASQUERADE rules
  const skipNatContainerRule =
    `iptables -t nat -C POSTROUTING -s ${containerSubnet} -d ${clusterCidr} -j RETURN 2>/dev/null || iptables -t nat -I POSTROUTING 1 -s ${containerSubnet} -d ${clusterCidr} -j RETURN`;
  const result3b = await ssh.executeCommand(skipNatContainerRule);
  if (result3b.code !== 0 && !result3b.stderr.includes("Bad rule")) {
    log.warn(
      `Failed to add skip NAT rule for container traffic on ${host}`,
      "network",
    );
  }

  // MASQUERADE only for internet-bound traffic (not going over WireGuard)
  // This allows containers to access the internet while preserving source IPs for cluster communication
  const internetMasqueradeRule =
    `iptables -t nat -C POSTROUTING -s ${localSubnet} ! -o ${wireguardInterface} -j MASQUERADE 2>/dev/null || iptables -t nat -A POSTROUTING -s ${localSubnet} ! -o ${wireguardInterface} -j MASQUERADE`;
  const result4 = await ssh.executeCommand(internetMasqueradeRule);
  if (result4.code !== 0 && !result4.stderr.includes("Bad rule")) {
    log.warn(
      `Failed to add internet MASQUERADE rule on ${host}`,
      "network",
    );
  }

  // Allow established connections
  const establishedRule =
    `iptables -C FORWARD -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT 2>/dev/null || iptables -A FORWARD -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT`;
  await ssh.executeCommand(establishedRule);

  // Save iptables rules
  const saveResult = await ssh.executeCommand(
    "iptables-save > /etc/iptables/rules.v4 2>/dev/null || netfilter-persistent save 2>/dev/null || true",
  );

  if (saveResult.code === 0) {
    log.debug(`iptables rules saved on ${host}`, "network");
  } else {
    log.warn(`Could not persist iptables rules on ${host}`, "network");
  }
}

/**
 * Setup complete routing configuration for a server
 *
 * This is the main entry point that configures all routing aspects:
 * - IP forwarding
 * - Routes to peer subnets
 * - iptables forwarding rules
 *
 * @param ssh - SSH connection to the server
 * @param localSubnet - Local WireGuard subnet CIDR
 * @param containerSubnet - Local container subnet CIDR
 * @param clusterCidr - Cluster-wide CIDR for all containers
 * @param peers - Array of peer information
 * @param networkName - Docker network name
 * @param engine - Container engine (docker or podman)
 * @param wireguardInterface - WireGuard interface name (default: jiji0)
 */
export async function setupServerRouting(
  ssh: SSHManager,
  localSubnet: string,
  containerSubnet: string,
  clusterCidr: string,
  peers: Array<{ subnet: string; hostname: string }>,
  networkName: string,
  engine: "docker" | "podman",
  wireguardInterface = "jiji0",
): Promise<void> {
  const host = ssh.getHost();

  try {
    // Enable IP forwarding
    await enableIPForwarding(ssh);

    // Add routes to peer subnets
    if (peers.length > 0) {
      await addPeerRoutes(ssh, peers, wireguardInterface);
    }

    // Get Docker bridge interface name
    const dockerBridge = await getDockerBridgeInterface(
      ssh,
      networkName,
      engine,
    );

    // Configure iptables rules
    await setupIPTablesRules(
      ssh,
      localSubnet,
      containerSubnet,
      clusterCidr,
      dockerBridge,
      wireguardInterface,
    );
  } catch (error) {
    throw new Error(`Failed to setup routing on ${host}: ${error}`);
  }
}

/**
 * Clean up routing configuration
 *
 * @param ssh - SSH connection to the server
 * @param subnets - Subnets to remove routes for
 * @param dockerBridge - Docker bridge interface
 * @param wireguardInterface - WireGuard interface name
 */
export async function cleanupServerRouting(
  ssh: SSHManager,
  subnets: string[],
  _dockerBridge: string,
  _wireguardInterface = "jiji0",
): Promise<void> {
  const host = ssh.getHost();

  // Remove peer routes
  await removePeerRoutes(ssh, subnets);

  // Remove iptables rules (optional - might affect other services)
  // We'll skip this for now to be safe

  log.debug(`Routing cleanup completed on ${host}`, "network");
}

/**
 * Verify routing configuration
 *
 * @param ssh - SSH connection to the server
 * @param expectedRoutes - Expected peer subnets
 * @param wireguardInterface - WireGuard interface name
 * @returns True if all routes are configured
 */
export async function verifyRouting(
  ssh: SSHManager,
  expectedRoutes: string[],
  wireguardInterface = "jiji0",
): Promise<boolean> {
  const result = await ssh.executeCommand("ip route show");

  if (result.code !== 0) {
    return false;
  }

  const routes = result.stdout;

  for (const subnet of expectedRoutes) {
    const routeExists = routes.includes(`${subnet} dev ${wireguardInterface}`);
    if (!routeExists) {
      log.warn(
        `Missing route to ${subnet} on ${ssh.getHost()}`,
        "network",
      );
      return false;
    }
  }

  return true;
}
