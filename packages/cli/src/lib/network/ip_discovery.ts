/**
 * IP address discovery utilities for WireGuard endpoints
 *
 * Discovers public and private IP addresses to enable NAT traversal
 * and multi-homed server configurations.
 */

import type { SSHManager } from "../../utils/ssh.ts";
import { log } from "../../utils/logger.ts";

/**
 * Public IP discovery services
 * Using multiple services for redundancy
 */
const PUBLIC_IP_SERVICES = [
  "https://api.ipify.org",
  "https://ipinfo.io/ip",
  "https://icanhazip.com",
];

/**
 * Discover the public IP address of a server
 *
 * Queries multiple external services to determine the public-facing IP.
 * This is essential for NAT traversal as it provides the IP that other
 * servers will need to use to reach this server through the internet.
 *
 * @param ssh - SSH connection to the server
 * @returns Public IP address or null if discovery fails
 */
export async function discoverPublicIP(
  ssh: SSHManager,
): Promise<string | null> {
  const _host = ssh.getHost();

  for (const service of PUBLIC_IP_SERVICES) {
    try {
      const result = await ssh.executeCommand(
        `curl -s --max-time 5 "${service}"`,
      );

      if (result.code === 0) {
        const ip = result.stdout.trim();

        // Validate IP format (basic IPv4 check)
        if (/^\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}$/.test(ip)) {
          log.debug(`Discovered public IP ${ip} via ${service}`, "network");
          return ip;
        }
      }
    } catch (error) {
      log.debug(
        `Failed to discover public IP from ${service}: ${error}`,
        "network",
      );
      // Continue to next service
    }
  }

  return null;
}

/**
 * Discover private IP addresses from network interfaces
 *
 * Scans all network interfaces and returns routable private IPs,
 * filtering out Docker bridges, loopback, and non-running interfaces.
 *
 * @param ssh - SSH connection to the server
 * @returns Array of private IP addresses
 */
export async function discoverPrivateIPs(
  ssh: SSHManager,
): Promise<string[]> {
  const host = ssh.getHost();
  const privateIps: string[] = [];

  try {
    // Get all IP addresses from network interfaces
    const result = await ssh.executeCommand(
      `ip -4 addr show | grep -oP '(?<=inet\\s)\\d+(\\.\\d+){3}'`,
    );

    if (result.code !== 0) {
      log.warn(
        `Failed to discover private IPs on ${host}: ${result.stderr}`,
        "network",
      );
      return [];
    }

    const ips = result.stdout.trim().split("\n");

    // Filter out loopback and get interface details for each IP
    for (const ip of ips) {
      if (!ip || ip === "127.0.0.1") {
        continue;
      }

      // Skip non private IPs (e.g., public IPs configured on interfaces)
      if (!isPrivateIP(ip)) {
        continue;
      }

      // Get interface name for this IP
      // The interface name is at the end of the inet line
      const ifaceResult = await ssh.executeCommand(
        `ip addr show | grep "inet ${ip}" | awk '{print $NF}'`,
      );

      if (ifaceResult.code === 0) {
        const iface = ifaceResult.stdout.trim();

        // Skip Docker/Podman bridges and WireGuard interfaces
        if (
          iface.startsWith("docker") ||
          iface.startsWith("podman") ||
          iface.startsWith("br-") ||
          iface.startsWith("cni-") ||
          iface.startsWith("jiji") ||
          iface.startsWith("wg")
        ) {
          continue;
        }

        // Check if interface is up
        const stateResult = await ssh.executeCommand(
          `ip link show ${iface} | grep -q "state UP" && echo "up" || echo "down"`,
        );

        if (stateResult.stdout.trim() === "up") {
          privateIps.push(ip);
        }
      }
    }

    if (privateIps.length > 0) {
      log.debug(
        `Discovered private IPs on ${host}: ${privateIps.join(", ")}`,
        "network",
      );
    }
  } catch (error) {
    log.warn(
      `Error discovering private IPs on ${host}: ${error}`,
      "network",
    );
  }

  return privateIps;
}

/**
 * Discover all endpoints (public and private) for a server
 *
 * Combines public IP discovery and private IP discovery to create
 * a list of endpoints that other servers can try when
 * establishing WireGuard connections.
 *
 * @param ssh - SSH connection to the server
 * @param port - WireGuard listen port (default: 31820)
 * @returns Array of endpoints in "IP:PORT" format
 */
export async function discoverAllEndpoints(
  ssh: SSHManager,
  port: number = 31820,
): Promise<string[]> {
  const endpoints: string[] = [];

  // Discover public IP
  const publicIp = await discoverPublicIP(ssh);
  if (publicIp) {
    endpoints.push(`${publicIp}:${port}`);
  }

  // Discover private IPs
  const privateIps = await discoverPrivateIPs(ssh);
  for (const privateIp of privateIps) {
    endpoints.push(`${privateIp}:${port}`);
  }

  // If no IPs discovered, fall back to hostname
  if (endpoints.length === 0) {
    const hostname = ssh.getHost();
    endpoints.push(`${hostname}:${port}`);
    log.warn(
      `No IPs discovered for ${hostname}, using hostname as endpoint`,
      "network",
    );
  }

  return endpoints;
}

/**
 * Validate if an IP address is in private range
 *
 * @param ip - IP address to check
 * @returns True if IP is in a private range (10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16)
 */
export function isPrivateIP(ip: string): boolean {
  const octets = ip.split(".").map((octet) => parseInt(octet, 10));

  if (octets.length !== 4 || octets.some((octet) => isNaN(octet))) {
    return false;
  }

  // 10.0.0.0/8
  if (octets[0] === 10) {
    return true;
  }

  // 172.16.0.0/12
  if (octets[0] === 172 && octets[1] >= 16 && octets[1] <= 31) {
    return true;
  }

  // 192.168.0.0/16
  if (octets[0] === 192 && octets[1] === 168) {
    return true;
  }

  return false;
}

/**
 * Parse endpoint string to extract IP and port
 *
 * @param endpoint - Endpoint string in format "IP:PORT" or "HOSTNAME:PORT"
 * @returns Object with host and port, or null if invalid format
 */
export function parseEndpoint(
  endpoint: string,
): { host: string; port: number } | null {
  const match = endpoint.match(/^(.+):(\d+)$/);
  if (!match) {
    return null;
  }

  const host = match[1];
  const port = parseInt(match[2], 10);

  if (isNaN(port) || port < 1 || port > 65535) {
    return null;
  }

  return { host, port };
}

/**
 * Select the best WireGuard endpoint based on network locality
 *
 * Prioritizes direct private network connections when both servers are on
 * the same local network (e.g., 192.168.1.x). Falls back to public IP for
 * cross-network connections.
 *
 * @param sourceEndpoints - Endpoints of the source server (this server)
 * @param targetEndpoints - Endpoints of the target peer
 * @returns Best endpoint to use for connecting to the peer
 */
export function selectBestEndpoint(
  sourceEndpoints: string[],
  targetEndpoints: string[],
): string {
  // Extract IPs from endpoints (remove :port)
  const sourceIPs = sourceEndpoints.map((ep) => ep.split(":")[0]);

  // Check if both servers have private IPs in the same /24 subnet
  for (const sourceIP of sourceIPs) {
    if (!isPrivateIP(sourceIP)) continue;

    const sourceSubnet = sourceIP.split(".").slice(0, 3).join("."); // e.g., "192.168.1"

    for (const targetEndpoint of targetEndpoints) {
      const targetIP = targetEndpoint.split(":")[0];
      if (!isPrivateIP(targetIP)) continue;

      const targetSubnet = targetIP.split(".").slice(0, 3).join(".");

      // Same /24 subnet - use private IP for direct connection
      if (sourceSubnet === targetSubnet) {
        log.debug(
          `Using local endpoint ${targetEndpoint} (same subnet: ${sourceSubnet}.x)`,
          "network",
        );
        return targetEndpoint;
      }
    }
  }

  // No matching private subnets - use first endpoint (public IP)
  log.debug(
    `Using public endpoint ${targetEndpoints[0]} (no shared local network)`,
    "network",
  );
  return targetEndpoints[0];
}
