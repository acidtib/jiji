/**
 * Tests for tombstone-based server removal.
 *
 * Covers tombstoneServer() behavior over a mock SSH session, and schema-level
 * regression checks that ensure every server-table reader still filters by
 * removed_at (so a partial Corrosion view can't re-introduce the split-brain
 * peer-eviction bug we fixed).
 */

import { assertEquals } from "@std/assert";
import { MockSSHManager } from "./mocks.ts";
import { tombstoneServer } from "../src/lib/network/corrosion.ts";

// deno-lint-ignore no-explicit-any
const asSsh = (mock: MockSSHManager): any => mock;

Deno.test("tombstoneServer - returns not-found when row doesn't exist", async () => {
  const mockSsh = new MockSSHManager("test-host");

  // SELECT id, IFNULL(removed_at, 0) returns empty stdout for no row.
  mockSsh.addMockResponse("SELECT id, IFNULL(removed_at, 0)", {
    success: true,
    stdout: "",
    stderr: "",
    code: 0,
  });

  const result = await tombstoneServer(asSsh(mockSsh), "ghost-server");

  assertEquals(result, { tombstoned: false, alreadyRemoved: false });

  // We must not have issued an UPDATE for a row that doesn't exist.
  const updates = mockSsh.getAllCommands().filter((c) =>
    c.includes("UPDATE servers SET removed_at")
  );
  assertEquals(updates.length, 0);
});

Deno.test("tombstoneServer - no-op when row already tombstoned", async () => {
  const mockSsh = new MockSSHManager("test-host");

  // Row exists, removed_at = 1700000000000 (already tombstoned).
  mockSsh.addMockResponse("SELECT id, IFNULL(removed_at, 0)", {
    success: true,
    stdout: "caja03|1700000000000",
    stderr: "",
    code: 0,
  });

  const result = await tombstoneServer(asSsh(mockSsh), "caja03");

  assertEquals(result, { tombstoned: true, alreadyRemoved: true });

  // No UPDATE should fire — the row is already tombstoned and we must not
  // overwrite the original timestamp.
  const updates = mockSsh.getAllCommands().filter((c) =>
    c.includes("UPDATE servers SET removed_at")
  );
  assertEquals(updates.length, 0);
});

Deno.test("tombstoneServer - writes tombstone for active row", async () => {
  const mockSsh = new MockSSHManager("test-host");

  // Row exists, removed_at IS NULL → IFNULL returns 0.
  mockSsh.addMockResponse("SELECT id, IFNULL(removed_at, 0)", {
    success: true,
    stdout: "caja03|0",
    stderr: "",
    code: 0,
  });

  // The UPDATE goes via the curl-to-corrosion-API path; succeed silently.
  mockSsh.addMockResponse("UPDATE servers SET removed_at", {
    success: true,
    stdout: "",
    stderr: "",
    code: 0,
  });

  const result = await tombstoneServer(asSsh(mockSsh), "caja03");

  assertEquals(result, { tombstoned: true, alreadyRemoved: false });

  const updates = mockSsh.getAllCommands().filter((c) =>
    c.includes("UPDATE servers SET removed_at")
  );
  assertEquals(updates.length, 1);
  // The UPDATE must include the WHERE id = ... AND removed_at IS NULL guard
  // so a concurrent tombstone doesn't get clobbered.
  assertEquals(
    updates[0].includes("removed_at IS NULL"),
    true,
    `expected concurrent-write guard in UPDATE, got: ${updates[0]}`,
  );
});

// --- Schema-level regression tests ---------------------------------------
// These read source files and assert on substrings, matching the existing
// pattern in network_resilience_test.ts. The point is to catch regressions
// where someone adds a new server-table reader without filtering by
// removed_at, which would re-open the door to the split-brain peer eviction.

Deno.test("CORROSION_SCHEMA includes removed_at tombstone column", async () => {
  const content = await Deno.readTextFile("src/lib/network/corrosion.ts");
  assertEquals(
    content.includes("removed_at INTEGER DEFAULT NULL"),
    true,
    "Missing removed_at column in servers schema",
  );
});

Deno.test("registerServer preserves removed_at on conflict", async () => {
  const content = await Deno.readTextFile("src/lib/network/corrosion.ts");
  // The UPSERT in registerServer must NOT touch removed_at. If a future
  // change reverts to INSERT OR REPLACE (or adds removed_at to the SET list),
  // re-running `jiji network setup` would silently un-tombstone servers.
  assertEquals(
    content.includes("ON CONFLICT(id) DO UPDATE SET"),
    true,
    "registerServer must use UPSERT, not INSERT OR REPLACE",
  );
  const upsertStart = content.indexOf("ON CONFLICT(id) DO UPDATE SET");
  const upsertSection = content.slice(upsertStart, upsertStart + 500);
  assertEquals(
    upsertSection.includes("removed_at"),
    false,
    "registerServer UPSERT must not write removed_at — it would clobber tombstones",
  );
});

Deno.test("queryActiveServers excludes tombstoned rows", async () => {
  const content = await Deno.readTextFile("src/lib/network/corrosion.ts");
  const fnStart = content.indexOf("export async function queryActiveServers");
  const fnSection = content.slice(fnStart, fnStart + 800);
  assertEquals(
    fnSection.includes("removed_at IS NULL"),
    true,
    "queryActiveServers must filter removed_at IS NULL",
  );
});

Deno.test("queryAllServers excludes tombstoned rows by default", async () => {
  const content = await Deno.readTextFile("src/lib/network/corrosion.ts");
  const fnStart = content.indexOf("export async function queryAllServers");
  const fnSection = content.slice(fnStart, fnStart + 1000);
  assertEquals(
    fnSection.includes("includeRemoved"),
    true,
    "queryAllServers must support an includeRemoved flag",
  );
  assertEquals(
    fnSection.includes("removed_at IS NULL"),
    true,
    "queryAllServers must filter removed_at IS NULL by default",
  );
});

Deno.test("queryOfflineServers excludes tombstoned rows", async () => {
  const content = await Deno.readTextFile("src/lib/network/corrosion.ts");
  const fnStart = content.indexOf("export async function queryOfflineServers");
  const fnSection = content.slice(fnStart, fnStart + 800);
  assertEquals(
    fnSection.includes("removed_at IS NULL"),
    true,
    "queryOfflineServers must filter removed_at IS NULL",
  );
});
