/**
 * Network helper utilities
 */

import type { SSHManager } from "./ssh.ts";
import { getServerByHostname, loadTopology } from "../lib/network/topology.ts";
import { log } from "./logger.ts";

/**
 * Get DNS server IP for a specific host from network topology
 *
 * @param ssh SSH manager for the host
 * @param hostname Hostname to get DNS server for
 * @param networkEnabled Whether the network is enabled
 * @returns DNS server IP or undefined if not available
 *
 * @example
 * ```typescript
 * const dnsServer = await getDnsServerForHost(hostSsh, "app-1", config.network.enabled);
 * if (dnsServer) {
 *   builder.dns(dnsServer, config.network.serviceDomain);
 * }
 * ```
 */
export async function getDnsServerForHost(
  ssh: SSHManager,
  hostname: string,
  networkEnabled: boolean,
): Promise<string | undefined> {
  if (!networkEnabled) {
    return undefined;
  }

  try {
    const topology = await loadTopology(ssh);
    if (!topology) {
      log.debug(
        `Network topology not found for ${hostname}`,
        "network",
      );
      return undefined;
    }

    const server = getServerByHostname(topology, hostname);
    if (!server) {
      log.debug(
        `Server ${hostname} not found in network topology`,
        "network",
      );
      return undefined;
    }

    return server.wireguardIp;
  } catch (error) {
    log.debug(
      `Failed to get DNS server for ${hostname}: ${error}`,
      "network",
    );
    return undefined;
  }
}

/**
 * Get DNS server IPs for multiple hosts from network topology
 *
 * @param sshManagers SSH managers for hosts
 * @param networkEnabled Whether the network is enabled
 * @returns Map of hostname to DNS server IP
 *
 * @example
 * ```typescript
 * const dnsServers = await getDnsServersForHosts(sshManagers, config.network.enabled);
 * const dnsServer = dnsServers.get(host);
 * ```
 */
export async function getDnsServersForHosts(
  sshManagers: SSHManager[],
  networkEnabled: boolean,
): Promise<Map<string, string>> {
  const dnsServers = new Map<string, string>();

  if (!networkEnabled) {
    return dnsServers;
  }

  await Promise.all(
    sshManagers.map(async (ssh) => {
      const hostname = ssh.getHost();
      const dnsServer = await getDnsServerForHost(ssh, hostname, true);
      if (dnsServer) {
        dnsServers.set(hostname, dnsServer);
      }
    }),
  );

  return dnsServers;
}
