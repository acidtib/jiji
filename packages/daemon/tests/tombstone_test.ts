/**
 * Schema-level regression tests for tombstone-aware reconciliation.
 *
 * These read source files and assert on key substrings, matching the
 * existing test pattern in this package. The point is to catch regressions
 * where someone adds (or reverts) reconciliation logic without preserving
 * the rules that prevent split-brain peer eviction:
 *
 *   1. The reconciler only treats a non-null `removed_at` as evidence to
 *      remove a peer. Absence from the active set is NEVER a removal signal.
 *   2. The reconciler must not filter peer candidates by `last_seen` —
 *      that turned brief outages into permanent mesh tears.
 *   3. The GC and split-brain detector must skip tombstoned rows so an
 *      explicitly-decommissioned server doesn't trigger spurious alerts or
 *      offline-server cleanup logic.
 */

import { assertEquals, assertStringIncludes } from "@std/assert";

Deno.test("peer_reconciler reads non-tombstoned peer servers", async () => {
  const content = await Deno.readTextFile("src/peer_reconciler.ts");
  const fnStart = content.indexOf("async function getPeerServers");
  assertEquals(
    fnStart >= 0,
    true,
    "getPeerServers helper must exist",
  );
  const fnSection = content.slice(fnStart, fnStart + 600);
  assertStringIncludes(fnSection, "removed_at IS NULL");
  // No last_seen filter — a temporarily-offline peer must remain a WG peer
  // so gossip can resume when it returns. This was the original bug.
  assertEquals(
    fnSection.includes("last_seen"),
    false,
    "getPeerServers must not filter on last_seen (caused split-brain)",
  );
});

Deno.test("peer_reconciler reads tombstoned pubkeys for explicit removal", async () => {
  const content = await Deno.readTextFile("src/peer_reconciler.ts");
  const fnStart = content.indexOf("async function getTombstonedPubkeys");
  assertEquals(
    fnStart >= 0,
    true,
    "getTombstonedPubkeys helper must exist",
  );
  const fnSection = content.slice(fnStart, fnStart + 400);
  assertStringIncludes(fnSection, "removed_at IS NOT NULL");
});

Deno.test("peer_reconciler removes peers only via positive tombstone evidence", async () => {
  const content = await Deno.readTextFile("src/peer_reconciler.ts");
  // The reconcile loop must only call wg.removePeer when the pubkey is in
  // the tombstoned set. Guard against a regression to absence-based removal.
  assertStringIncludes(content, "tombstonedPubkeys.has(peer.publicKey)");
  assertEquals(
    content.includes("activePubkeys.has"),
    false,
    "Reconciler must not key peer removal on the active-peer set (split-brain bug)",
  );
});

Deno.test("garbage_collector immediately deletes tombstoned-server containers", async () => {
  const content = await Deno.readTextFile("src/garbage_collector.ts");
  const fnStart = content.indexOf(
    "async function deleteTombstonedServerContainers",
  );
  assertEquals(
    fnStart >= 0,
    true,
    "deleteTombstonedServerContainers helper must exist",
  );
  const fnSection = content.slice(fnStart, fnStart + 600);
  assertStringIncludes(fnSection, "removed_at IS NOT NULL");
});

Deno.test("garbage_collector offline-server cleanup skips tombstoned rows", async () => {
  const content = await Deno.readTextFile("src/garbage_collector.ts");
  const fnStart = content.indexOf(
    "async function deleteOfflineServerContainers",
  );
  const fnSection = content.slice(fnStart, fnStart + 600);
  // Offline cleanup uses a stale-heartbeat threshold. It must not also fire
  // on tombstoned servers — those are handled by the dedicated path above,
  // and we don't want the GC double-counting them.
  assertStringIncludes(fnSection, "removed_at IS NULL");
});

Deno.test("split_brain counts ignore tombstoned servers", async () => {
  const content = await Deno.readTextFile("src/split_brain.ts");
  // Both totals (total servers, active servers, unreachable list) must
  // exclude tombstoned rows. Otherwise a decommissioned server permanently
  // skews the active/total ratio toward a false split-brain alert.
  const matches = content.match(/removed_at IS NULL/g) ?? [];
  assertEquals(
    matches.length >= 3,
    true,
    `expected >= 3 'removed_at IS NULL' guards in split_brain.ts, got ${matches.length}`,
  );
});
