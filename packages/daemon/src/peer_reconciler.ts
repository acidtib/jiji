/**
 * Reconcile WireGuard peers with Corrosion state.
 *
 * Additive by default: adds missing peers, fixes incorrect AllowedIPs, and
 * removes peers only when their server row is explicitly tombstoned
 * (removed_at IS NOT NULL). Absence from the Corrosion view is never treated
 * as a removal signal — that turns transient gossip partitions into permanent
 * mesh tears.
 */

import type { Config, ServerRecord } from "./types.ts";
import type { CorrosionCli } from "./corrosion_cli.ts";
import * as wg from "./wireguard.ts";
import {
  isValidCIDR,
  isValidEndpoint,
  isValidIPv6,
  isValidWireGuardKey,
  sql,
} from "./validation.ts";
import { type CachedPeer, saveCache } from "./peer_cache.ts";
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
 * Get all non-tombstoned peer servers from Corrosion.
 *
 * We deliberately do NOT filter by last_seen: a server being temporarily
 * unreachable is the exact scenario where we most need to keep its WireGuard
 * peer in place so gossip can resume on recovery.
 */
async function getPeerServers(
  cli: CorrosionCli,
  serverId: string,
): Promise<ServerRecord[]> {
  const rows = await cli.query(
    sql`SELECT wireguard_pubkey, subnet, management_ip, endpoints FROM servers WHERE removed_at IS NULL AND id != ${serverId};`,
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
 * Get pubkeys of tombstoned servers. These should be torn down from
 * WireGuard on next reconcile cycle, cluster-wide.
 */
async function getTombstonedPubkeys(
  cli: CorrosionCli,
  serverId: string,
): Promise<Set<string>> {
  const rows = await cli.query(
    sql`SELECT wireguard_pubkey FROM servers WHERE removed_at IS NOT NULL AND id != ${serverId};`,
  );
  return new Set(rows.map((r) => r[0]).filter((k): k is string => !!k?.trim()));
}

/**
 * Reconcile WireGuard peers with the current Corrosion state.
 */
export async function reconcilePeers(
  config: Config,
  cli: CorrosionCli,
): Promise<void> {
  log.info("Reconciling WireGuard peers...");

  const peerServers = await getPeerServers(cli, config.serverId);
  const tombstonedPubkeys = await getTombstonedPubkeys(cli, config.serverId);

  if (peerServers.length === 0 && tombstonedPubkeys.size === 0) {
    // No peers and nothing to remove. This is normal on a fresh single-node
    // cluster; otherwise it usually means Corrosion is empty/uninitialized.
    log.warn("No peer servers found in Corrosion");
    return;
  }

  // Get current WireGuard peers
  const currentPeers = await wg.showDump(config.interfaceName);
  const currentPubkeys = new Set(currentPeers.map((p) => p.publicKey));

  // Build the cache snapshot as we validate each server below. Only peers
  // that pass every validation gate make it in — we don't want a stale
  // cache to seed something `wg` would reject.
  const cacheable: CachedPeer[] = [];

  // Add missing peers or fix incorrect allowed IPs
  for (const server of peerServers) {
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

    // Compute expected allowed IPs including container subnet
    const expectedAllowedIps = computeExpectedAllowedIps(server);

    // Server passed all validation gates — record it for the boot cache.
    cacheable.push({
      publicKey: server.wireguardPubkey,
      allowedIps: expectedAllowedIps,
      endpoints: server.endpoints,
      persistentKeepalive: 25,
    });

    if (!currentPubkeys.has(server.wireguardPubkey)) {
      // New peer — add it
      log.info("Adding new peer", { pubkey: server.wireguardPubkey });

      try {
        await wg.setPeer(config.interfaceName, {
          publicKey: server.wireguardPubkey,
          allowedIps: expectedAllowedIps,
          endpoint,
          persistentKeepalive: 25,
        });
      } catch (err) {
        log.error("Failed to add peer", {
          pubkey: server.wireguardPubkey,
          error: String(err),
        });
      }
    } else {
      // Existing peer — check if allowed IPs are correct
      const existingPeer = currentPeers.find(
        (p) => p.publicKey === server.wireguardPubkey,
      );
      if (existingPeer) {
        const currentIps = normalizeAllowedIps(existingPeer.allowedIps);
        const expectedIps = normalizeAllowedIps(expectedAllowedIps);

        if (currentIps !== expectedIps) {
          log.warn("Fixing incorrect allowed IPs for peer", {
            pubkey: server.wireguardPubkey,
            current: currentIps,
            expected: expectedIps,
          });

          try {
            await wg.setPeer(config.interfaceName, {
              publicKey: server.wireguardPubkey,
              allowedIps: expectedAllowedIps,
              endpoint: existingPeer.endpoint !== "(none)"
                ? existingPeer.endpoint
                : endpoint,
              persistentKeepalive: 25,
            });
          } catch (err) {
            log.error("Failed to fix peer allowed IPs", {
              pubkey: server.wireguardPubkey,
              error: String(err),
            });
          }
        }
      }
    }
  }

  // Remove peers that have been explicitly tombstoned. We only remove on
  // positive evidence (a non-null removed_at in Corrosion), never on absence
  // from the active set — that would re-introduce the split-brain bug where a
  // partition causes nodes to evict each other.
  for (const peer of currentPeers) {
    if (tombstonedPubkeys.has(peer.publicKey)) {
      log.warn("Removing tombstoned peer", { pubkey: peer.publicKey });
      try {
        await wg.removePeer(config.interfaceName, peer.publicKey);
      } catch (err) {
        log.error("Failed to remove tombstoned peer", {
          pubkey: peer.publicKey,
          error: String(err),
        });
      }
    }
  }

  // Snapshot the validated peer set so a future cold boot can seed WG
  // without needing Corrosion to be reachable. Guard against writing an
  // empty cache: if every peer failed validation, that's a bug, not a
  // legitimate "no peers" state — preserve whatever the previous cache had.
  if (cacheable.length > 0) {
    try {
      await saveCache(config.peerCachePath, cacheable);
    } catch (err) {
      log.warn("Failed to write peer cache", {
        path: config.peerCachePath,
        error: String(err),
      });
    }
  }
}

/**
 * Compute the expected allowed IPs for a peer, including its container subnet.
 *
 * Given a server subnet like 10.210.1.0/24, the container subnet is
 * 10.210.(128 + serverIndex).0/24 where serverIndex is the third octet.
 */
function computeExpectedAllowedIps(server: ServerRecord): string {
  const parts = server.subnet.split(".");
  const serverIndex = parseInt(parts[2]);
  const baseNetwork = parts.slice(0, 2).join(".");
  const containerSubnet = `${baseNetwork}.${128 + serverIndex}.0/24`;

  return `${server.subnet},${containerSubnet},${server.managementIp}/128`;
}

/**
 * Normalize an IPv6 address by expanding all groups to 4 hex digits.
 * This ensures consistent comparison between compressed (from WireGuard)
 * and uncompressed (from Corrosion) forms.
 */
function expandIpv6(addr: string): string {
  // Split off the prefix length if present (e.g., /128)
  const [ip, prefix] = addr.split("/");

  // Expand :: shorthand
  const halves = ip.split("::");
  let groups: string[];
  if (halves.length === 2) {
    const left = halves[0] ? halves[0].split(":") : [];
    const right = halves[1] ? halves[1].split(":") : [];
    const missing = 8 - left.length - right.length;
    groups = [...left, ...Array(missing).fill("0"), ...right];
  } else {
    groups = ip.split(":");
  }

  // Pad each group to 4 digits
  const expanded = groups.map((g) => g.padStart(4, "0")).join(":");
  return prefix !== undefined ? `${expanded}/${prefix}` : expanded;
}

/**
 * Normalize allowed IPs string for comparison.
 * Sorts the IPs, removes whitespace, and expands IPv6 addresses
 * so compressed and uncompressed forms compare equal.
 */
function normalizeAllowedIps(ips: string): string {
  return ips
    .split(",")
    .map((ip) => ip.trim())
    .filter(Boolean)
    .map((ip) => (ip.includes(":") ? expandIpv6(ip) : ip))
    .sort()
    .join(",");
}
