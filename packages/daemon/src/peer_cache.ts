/**
 * On-disk cache of the WireGuard peer set.
 *
 * Purpose: break the chicken-and-egg between WireGuard and Corrosion. The
 * reconciler reads peers from Corrosion, but Corrosion gossip needs the
 * WireGuard mesh to function. If the daemon boots into a clean WG interface
 * (e.g. after `wg-quick down` + reboot, or a fresh install) and Corrosion
 * is reachable only via the very peers we don't have yet, we'd be stuck.
 *
 * Each successful reconcile snapshots the intended peer set to disk. At
 * daemon startup we seed WG from that snapshot before consulting Corrosion,
 * so the mesh is operational immediately.
 *
 * The cache is additive on read: we only call `wg set peer`, never
 * `wg removePeer`. Tombstone-driven removal happens in the normal
 * reconcile cycle once Corrosion is reachable. If the cache contains a
 * peer that's since been tombstoned cluster-wide, it'll come back briefly
 * and be removed on the next reconcile — acceptable.
 */

import * as wg from "./wireguard.ts";
import {
  isValidCIDR,
  isValidEndpoint,
  isValidWireGuardKey,
} from "./validation.ts";
import * as log from "./logger.ts";

const SCHEMA_VERSION = 1;

export interface CachedPeer {
  publicKey: string;
  allowedIps: string;
  endpoints: string[];
  persistentKeepalive: number;
}

export interface PeerCacheFile {
  version: number;
  updatedAt: number;
  peers: CachedPeer[];
}

/**
 * Load and parse the cache file. Returns null when the file is missing,
 * unreadable, malformed, or carries a future schema version — never throws.
 * Callers should treat a null as "no cache available" and proceed.
 */
export async function loadCache(
  path: string,
): Promise<PeerCacheFile | null> {
  let text: string;
  try {
    text = await Deno.readTextFile(path);
  } catch (err) {
    if (err instanceof Deno.errors.NotFound) {
      return null;
    }
    log.warn("Failed to read peer cache, ignoring", {
      path,
      error: String(err),
    });
    return null;
  }

  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch (err) {
    log.warn("Peer cache is not valid JSON, ignoring", {
      path,
      error: String(err),
    });
    return null;
  }

  if (!isPeerCacheFile(parsed)) {
    log.warn("Peer cache failed shape validation, ignoring", { path });
    return null;
  }

  if (parsed.version > SCHEMA_VERSION) {
    log.warn("Peer cache schema version is newer than supported, ignoring", {
      path,
      cache_version: parsed.version,
      supported_version: SCHEMA_VERSION,
    });
    return null;
  }

  return parsed;
}

/**
 * Atomically persist the peer set. Writes to `<path>.tmp` then renames over
 * the target so a crash mid-write can never leave a corrupted file in place.
 * Creates parent directories as needed.
 */
export async function saveCache(
  path: string,
  peers: CachedPeer[],
): Promise<void> {
  const parent = parentDir(path);
  if (parent) {
    await Deno.mkdir(parent, { recursive: true });
  }

  const file: PeerCacheFile = {
    version: SCHEMA_VERSION,
    updatedAt: Date.now(),
    peers,
  };

  const tmp = `${path}.tmp`;
  await Deno.writeTextFile(tmp, JSON.stringify(file, null, 2), {
    mode: 0o640,
  });
  await Deno.rename(tmp, path);
}

/**
 * Seed WireGuard from the cache. Only adds peers that are missing on the
 * interface; never removes. Returns the number of peers actually set.
 * Invalid cached entries are skipped with a warning so a partially-bad
 * cache can't take down the whole hydrate.
 */
export async function hydrateFromCache(
  interfaceName: string,
  path: string,
): Promise<number> {
  const cache = await loadCache(path);
  if (!cache) {
    return 0;
  }

  let existing: Set<string>;
  try {
    const peers = await wg.showDump(interfaceName);
    existing = new Set(peers.map((p) => p.publicKey));
  } catch (err) {
    // If we can't even read WG state, the interface probably isn't up.
    // Bail out — hydrating would fail anyway.
    log.warn("Cannot read WireGuard state, skipping cache hydrate", {
      interface: interfaceName,
      error: String(err),
    });
    return 0;
  }

  let seeded = 0;
  for (const peer of cache.peers) {
    if (existing.has(peer.publicKey)) {
      continue;
    }

    if (!isValidCachedPeer(peer)) {
      log.warn("Skipping invalid peer in cache", {
        pubkey: peer.publicKey,
      });
      continue;
    }

    const endpoint = peer.endpoints[0];
    try {
      await wg.setPeer(interfaceName, {
        publicKey: peer.publicKey,
        allowedIps: peer.allowedIps,
        endpoint,
        persistentKeepalive: peer.persistentKeepalive,
      });
      seeded++;
    } catch (err) {
      log.warn("Failed to seed peer from cache", {
        pubkey: peer.publicKey,
        error: String(err),
      });
    }
  }

  return seeded;
}

function parentDir(path: string): string | null {
  const idx = path.lastIndexOf("/");
  if (idx <= 0) return null;
  return path.slice(0, idx);
}

function isPeerCacheFile(value: unknown): value is PeerCacheFile {
  if (!value || typeof value !== "object") return false;
  const v = value as Record<string, unknown>;
  if (typeof v.version !== "number") return false;
  if (typeof v.updatedAt !== "number") return false;
  if (!Array.isArray(v.peers)) return false;
  return v.peers.every(isCachedPeer);
}

function isCachedPeer(value: unknown): value is CachedPeer {
  if (!value || typeof value !== "object") return false;
  const p = value as Record<string, unknown>;
  if (typeof p.publicKey !== "string") return false;
  if (typeof p.allowedIps !== "string") return false;
  if (!Array.isArray(p.endpoints)) return false;
  if (!p.endpoints.every((e) => typeof e === "string")) return false;
  if (typeof p.persistentKeepalive !== "number") return false;
  return true;
}

/**
 * Stricter check used at hydrate time to keep malformed payloads out of `wg`.
 */
function isValidCachedPeer(peer: CachedPeer): boolean {
  if (!isValidWireGuardKey(peer.publicKey)) return false;
  if (peer.endpoints.length === 0) return false;
  if (!isValidEndpoint(peer.endpoints[0])) return false;
  // allowedIps is a comma-separated list; verify each entry is a CIDR.
  const allowed = peer.allowedIps.split(",").map((s) => s.trim()).filter(
    Boolean,
  );
  if (allowed.length === 0) return false;
  return allowed.every(isValidCIDR);
}
