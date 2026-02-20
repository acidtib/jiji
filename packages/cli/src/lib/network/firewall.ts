/**
 * UFW firewall configuration for container port forwarding
 *
 * Podman uses DNAT in iptables PREROUTING to forward host port traffic to
 * container IPs. UFW's `ufw allow` only adds INPUT chain rules, but forwarded
 * traffic goes through the FORWARD chain. Since UFW sets FORWARD to DROP by
 * default, container ports become unreachable externally.
 *
 * This module adds UFW forward rules in /etc/ufw/before.rules for the server's
 * container subnet, allowing forwarded traffic to reach containers.
 */

import type { SSHManager } from "../../utils/ssh.ts";
import { log } from "../../utils/logger.ts";

/**
 * Check if UFW is active on the server
 *
 * @param ssh - SSH connection to the server
 * @returns True if UFW is active
 */
export async function isUfwActive(ssh: SSHManager): Promise<boolean> {
  const result = await ssh.executeCommand("ufw status");

  if (result.code !== 0) {
    return false;
  }

  return result.stdout.includes("Status: active");
}

/**
 * Check if container forward rules already exist in /etc/ufw/before.rules
 *
 * @param ssh - SSH connection to the server
 * @param containerSubnet - Container subnet CIDR (e.g., "10.210.128.0/24")
 * @returns True if the forward rules are already present
 */
export async function hasContainerForwardRules(
  ssh: SSHManager,
  containerSubnet: string,
): Promise<boolean> {
  const result = await ssh.executeCommand(
    `grep -q 'jiji container forward rules for ${containerSubnet}' /etc/ufw/before.rules 2>/dev/null`,
  );

  return result.code === 0;
}

/**
 * Add container forward rules to /etc/ufw/before.rules
 *
 * Inserts ACCEPT rules for the container subnet in the *filter section's
 * ufw-before-forward chain, before the final COMMIT line. Then reloads UFW
 * to apply the changes.
 *
 * @param ssh - SSH connection to the server
 * @param containerSubnet - Container subnet CIDR (e.g., "10.210.128.0/24")
 */
export async function addContainerForwardRules(
  ssh: SSHManager,
  containerSubnet: string,
): Promise<void> {
  const host = ssh.getHost();

  // Validate containerSubnet is a proper CIDR to prevent sed injection
  if (!/^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+\/[0-9]+$/.test(containerSubnet)) {
    throw new Error(
      `Invalid container subnet CIDR format: ${containerSubnet}`,
    );
  }

  // Find the line number of the last COMMIT in before.rules (*filter section)
  const findResult = await ssh.executeCommand(
    `grep -n '^COMMIT$' /etc/ufw/before.rules | tail -1 | cut -d: -f1`,
  );

  if (findResult.code !== 0 || !findResult.stdout.trim()) {
    throw new Error(
      `Failed to find COMMIT line in /etc/ufw/before.rules on ${host}: ${findResult.stderr}`,
    );
  }

  const commitLine = findResult.stdout.trim();

  // Insert forward rules before the last COMMIT
  const rules = [
    `# jiji container forward rules for ${containerSubnet}`,
    `-A ufw-before-forward -s ${containerSubnet} -j ACCEPT`,
    `-A ufw-before-forward -d ${containerSubnet} -j ACCEPT`,
  ].join("\\n");

  const sedResult = await ssh.executeCommand(
    `sed -i '${commitLine}i\\${rules}' /etc/ufw/before.rules`,
  );

  if (sedResult.code !== 0) {
    throw new Error(
      `Failed to add UFW forward rules on ${host}: ${sedResult.stderr}`,
    );
  }

  // Reload UFW to apply changes
  const reloadResult = await ssh.executeCommand("ufw reload");

  if (reloadResult.code !== 0) {
    throw new Error(
      `Failed to reload UFW on ${host}: ${reloadResult.stderr}`,
    );
  }

  log.debug(
    `Added UFW container forward rules for ${containerSubnet} on ${host}`,
    "network",
  );
}

/**
 * Extract host-exposed ports from port mapping strings
 *
 * Parses port mappings and returns only those with explicit host ports
 * that are not bound to localhost (127.0.0.1). These are the ports
 * that need UFW rules for external access.
 *
 * @param ports - Array of port mapping strings (e.g., ["80:8080", "127.0.0.1:3000:3000"])
 * @returns Array of { port, protocol } for host-exposed ports
 */
export function extractHostPorts(
  ports: string[],
): Array<{ port: number; protocol: string }> {
  const result: Array<{ port: number; protocol: string }> = [];

  for (const mapping of ports) {
    // Extract protocol suffix
    const protocolMatch = mapping.match(/\/(tcp|udp)$/);
    const protocol = protocolMatch ? protocolMatch[1] : "tcp";
    const withoutProtocol = mapping.replace(/\/(tcp|udp)$/, "");

    const parts = withoutProtocol.split(":");

    if (parts.length === 2) {
      // "8080:8000" — host_port:container_port
      const hostPort = parseInt(parts[0], 10);
      if (!isNaN(hostPort) && hostPort > 0) {
        result.push({ port: hostPort, protocol });
      }
    } else if (parts.length === 3) {
      // "ip:host_port:container_port" — skip localhost-only bindings
      const hostIp = parts[0];
      if (hostIp === "127.0.0.1") continue;
      const hostPort = parseInt(parts[1], 10);
      if (!isNaN(hostPort) && hostPort > 0) {
        result.push({ port: hostPort, protocol });
      }
    }
    // Single port ("8000") has no host mapping, skip
  }

  return result;
}

/**
 * Ensure UFW forward rules are configured for the container subnet
 *
 * High-level function that detects UFW, checks for existing rules, and adds
 * them if needed. Safe to call multiple times (idempotent).
 *
 * @param ssh - SSH connection to the server
 * @param containerSubnet - Container subnet CIDR (e.g., "10.210.128.0/24")
 */
export async function ensureUfwForwardRules(
  ssh: SSHManager,
  containerSubnet: string,
): Promise<void> {
  const host = ssh.getHost();

  // Check if UFW is active
  const ufwActive = await isUfwActive(ssh);

  if (!ufwActive) {
    log.debug(
      `UFW is not active on ${host}, skipping forward rules`,
      "network",
    );
    return;
  }

  // Check if rules already exist
  const hasRules = await hasContainerForwardRules(ssh, containerSubnet);

  if (hasRules) {
    log.debug(
      `UFW forward rules for ${containerSubnet} already exist on ${host}`,
      "network",
    );
    return;
  }

  // Add the rules
  await addContainerForwardRules(ssh, containerSubnet);

  log.info(
    `UFW forward rules added for ${containerSubnet} on ${host}`,
    "network",
  );
}
