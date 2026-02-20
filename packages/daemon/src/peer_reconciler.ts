/**
 * Reconcile WireGuard peers with Corrosion state.
 *
 * Adds missing peers and removes stale ones.
 */

import type { Config, ServerRecord } from "./types.ts";
import type { CorrosionCli } from "./corrosion_cli.ts";
import * as wg from "./wireguard.ts";
import {
  escapeSql,
  isValidCIDR,
  isValidEndpoint,
  isValidIPv6,
  isValidWireGuardKey,
} from "./validation.ts";
import * as log from "./logger.ts";

/**
 * Parse endpoints JSON array from Corrosion query output.
 * Handles format like: ["1.2.3.4:31820","5.6.7.8:31820"]
 */
export function parseEndpoints(raw: string): string[] {
  try {
    const parsed = JSON.parse(raw);
    if (Array.isArray(parsed)) {
      return parsed.filter((e): e is string => typeof e === "string");
    }
  } catch {
    // Fall back to manual parsing for malformed JSON
  }
  return [];
}

/**
 * Get active servers from Corrosion (seen in last 5 minutes).
 */
async function getActiveServers(
  cli: CorrosionCli,
  serverId: string,
): Promise<ServerRecord[]> {
  const escapedId = escapeSql(serverId);
  const rows = await cli.query(
    `SELECT wireguard_pubkey, subnet, management_ip, endpoints FROM servers WHERE last_seen > (strftime('%s', 'now') - 300) * 1000 AND id != '${escapedId}';`,
  );

  return rows.filter((r) => r[0]?.trim()).map((row) => ({
    id: "",
    wireguardPubkey: row[0],
    subnet: row[1],
    managementIp: row[2],
    endpoints: parseEndpoints(row[3]),
    lastSeen: 0,
  }));
}

/**
 * Reconcile WireGuard peers with the current Corrosion state.
 */
export async function reconcilePeers(
  config: Config,
  cli: CorrosionCli,
): Promise<void> {
  log.info("Reconciling WireGuard peers...");

  const activeServers = await getActiveServers(cli, config.serverId);

  if (activeServers.length === 0) {
    log.warn("No active servers found in Corrosion");
    return;
  }

  // Get current WireGuard peers
  const currentPeers = await wg.showDump(config.interfaceName);
  const currentPubkeys = new Set(currentPeers.map((p) => p.publicKey));

  // Add missing peers
  for (const server of activeServers) {
    if (!currentPubkeys.has(server.wireguardPubkey)) {
      // Validate all Corrosion-sourced peer data before passing to wg
      if (!isValidWireGuardKey(server.wireguardPubkey)) {
        log.warn("Skipping peer with invalid pubkey format", {
          pubkey: server.wireguardPubkey,
        });
        continue;
      }

      if (!isValidCIDR(server.subnet)) {
        log.warn("Skipping peer with invalid subnet", {
          pubkey: server.wireguardPubkey,
          subnet: server.subnet,
        });
        continue;
      }

      if (!isValidIPv6(server.managementIp)) {
        log.warn("Skipping peer with invalid management IP", {
          pubkey: server.wireguardPubkey,
          management_ip: server.managementIp,
        });
        continue;
      }

      const endpoint = server.endpoints[0];
      if (!endpoint) {
        log.warn("Server has no endpoints, skipping", {
          pubkey: server.wireguardPubkey,
        });
        continue;
      }

      if (!isValidEndpoint(endpoint)) {
        log.warn("Skipping peer with invalid endpoint format", {
          pubkey: server.wireguardPubkey,
          endpoint,
        });
        continue;
      }

      log.info("Adding new peer", { pubkey: server.wireguardPubkey });

      try {
        await wg.setPeer(config.interfaceName, {
          publicKey: server.wireguardPubkey,
          allowedIps: `${server.subnet},${server.managementIp}/128`,
          endpoint,
          persistentKeepalive: 25,
        });
      } catch (err) {
        log.error("Failed to add peer", {
          pubkey: server.wireguardPubkey,
          error: String(err),
        });
      }
    }
  }

  // Remove stale peers (not in active servers list)
  const activePubkeys = new Set(
    activeServers.map((s) => s.wireguardPubkey),
  );
  for (const peer of currentPeers) {
    if (!activePubkeys.has(peer.publicKey)) {
      log.warn("Removing stale peer", { pubkey: peer.publicKey });
      try {
        await wg.removePeer(config.interfaceName, peer.publicKey);
      } catch (err) {
        log.error("Failed to remove peer", {
          pubkey: peer.publicKey,
          error: String(err),
        });
      }
    }
  }
}
