/**
 * Tests for the on-disk peer cache.
 *
 * The cache exists to break the chicken-and-egg between WireGuard and
 * Corrosion at daemon startup. These tests cover the file-format end:
 * round-trip serialization, atomic-write semantics, and graceful handling
 * of missing/malformed/future-version files.
 *
 * `hydrateFromCache` itself isn't unit-tested here because it shells out
 * to `wg`. Its file-loading half is exercised through `loadCache`.
 */

import { assert, assertEquals } from "@std/assert";
import { loadCache, saveCache } from "../src/peer_cache.ts";
import type { CachedPeer } from "../src/peer_cache.ts";

async function tempPath(): Promise<string> {
  const dir = await Deno.makeTempDir({ prefix: "jiji-peer-cache-test-" });
  return `${dir}/peers.json`;
}

const samplePeer: CachedPeer = {
  publicKey: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
  allowedIps: "10.210.1.0/24,10.210.129.0/24,fdb1::1/128",
  endpoints: ["1.2.3.4:31820", "192.168.1.220:31820"],
  persistentKeepalive: 25,
};

Deno.test("saveCache then loadCache round-trips peers", async () => {
  const path = await tempPath();
  try {
    await saveCache(path, [samplePeer]);
    const loaded = await loadCache(path);
    assert(loaded, "loadCache returned null after a fresh save");
    assertEquals(loaded.version, 1);
    assertEquals(loaded.peers, [samplePeer]);
    assert(loaded.updatedAt > 0, "updatedAt should be set");
  } finally {
    await Deno.remove(path, { recursive: false }).catch(() => {});
  }
});

Deno.test("saveCache creates parent directories", async () => {
  const dir = await Deno.makeTempDir({ prefix: "jiji-peer-cache-test-" });
  const path = `${dir}/nested/deep/peers.json`;
  try {
    await saveCache(path, [samplePeer]);
    const stat = await Deno.stat(path);
    assert(stat.isFile, "expected the cache file to exist");
  } finally {
    await Deno.remove(dir, { recursive: true }).catch(() => {});
  }
});

Deno.test("saveCache writes atomically (no .tmp residue on success)", async () => {
  const path = await tempPath();
  try {
    await saveCache(path, [samplePeer]);
    // The atomic-write path uses `<path>.tmp` then renames over the target.
    // A successful save must not leave the .tmp around.
    const tmpExists = await Deno.stat(`${path}.tmp`).then(() => true).catch(
      () => false,
    );
    assertEquals(tmpExists, false, "stale .tmp left after successful save");
  } finally {
    await Deno.remove(path).catch(() => {});
  }
});

Deno.test("loadCache returns null when file is missing", async () => {
  const result = await loadCache("/nonexistent/jiji/peers.json");
  assertEquals(result, null);
});

Deno.test("loadCache returns null for malformed JSON", async () => {
  const path = await tempPath();
  try {
    await Deno.writeTextFile(path, "{not valid json");
    const result = await loadCache(path);
    assertEquals(result, null);
  } finally {
    await Deno.remove(path).catch(() => {});
  }
});

Deno.test("loadCache returns null when shape doesn't match", async () => {
  const path = await tempPath();
  try {
    // Valid JSON, wrong shape — missing `peers` array.
    await Deno.writeTextFile(
      path,
      JSON.stringify({ version: 1, updatedAt: 0 }),
    );
    assertEquals(await loadCache(path), null);

    // peers entry missing required fields.
    await Deno.writeTextFile(
      path,
      JSON.stringify({
        version: 1,
        updatedAt: 0,
        peers: [{ publicKey: "x" }],
      }),
    );
    assertEquals(await loadCache(path), null);
  } finally {
    await Deno.remove(path).catch(() => {});
  }
});

Deno.test("loadCache refuses future schema versions", async () => {
  const path = await tempPath();
  try {
    // A forward-compatible read could silently accept fields it doesn't
    // understand and corrupt state. We'd rather log + fall through to
    // "no cache" and let the running daemon rebuild it.
    await Deno.writeTextFile(
      path,
      JSON.stringify({ version: 999, updatedAt: 0, peers: [] }),
    );
    assertEquals(await loadCache(path), null);
  } finally {
    await Deno.remove(path).catch(() => {});
  }
});

Deno.test("saveCache overwrites previous content", async () => {
  const path = await tempPath();
  try {
    await saveCache(path, [samplePeer]);
    await saveCache(path, []);
    const loaded = await loadCache(path);
    assert(loaded);
    assertEquals(loaded.peers, []);
  } finally {
    await Deno.remove(path).catch(() => {});
  }
});
