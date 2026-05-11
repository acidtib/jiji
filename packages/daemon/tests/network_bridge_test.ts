/**
 * Source-level regression guards for the daemon's bridge recovery.
 *
 * `reconcileNetworkBridge` shells out to `<engine>` and `ip` directly,
 * so a meaningful unit test would need to mock subprocess execution — out
 * of scope here. Instead pin the invariants that make the recovery
 * actually work: it doesn't silently swallow reload errors, it verifies
 * the bridge after reload, it has a recreate fallback, and it refuses
 * to wipe a network that has containers attached.
 */

import { assert, assertStringIncludes } from "@std/assert";

async function source(): Promise<string> {
  return await Deno.readTextFile("src/network_bridge.ts");
}

Deno.test("daemon bridge recovery verifies bridge after reload", async () => {
  const content = await source();
  // Must call bridgeExists() after reload, not assume reload's exit code
  // means the bridge came back (it doesn't, in the failure mode we saw).
  const reloadIdx = content.indexOf('"reload", "--all"');
  assert(reloadIdx >= 0, "reload --all call must exist");
  const after = content.slice(reloadIdx);
  assertStringIncludes(after, "bridgeExists(");
});

Deno.test("daemon bridge recovery has rm + create fallback", async () => {
  const content = await source();
  // Match the structural shape without depending on deno fmt's wrapping
  // (it splits multi-element arrays across lines).
  assertStringIncludes(content, '"network", "rm"');
  assertStringIncludes(content, "buildCreateArgs(");
  assertStringIncludes(content, '"create"');
});

Deno.test("daemon bridge recovery refuses when containers attached", async () => {
  const content = await source();
  // The presence of an attached-containers check + an early return when
  // the list is non-empty. Loose string match — looking for the guard
  // shape rather than exact wording.
  assertStringIncludes(content, "listAttachedContainers");
  assertStringIncludes(content, "attached.length > 0");
});

Deno.test("daemon bridge recovery refuses docker recreate (CLI-only)", async () => {
  const content = await source();
  // Docker recreate needs engine-specific options we can't always extract
  // from inspect output. The daemon must point the user at the CLI's
  // `jiji network setup`, not silently recreate with the wrong options.
  assertStringIncludes(content, 'engine === "docker"');
  assertStringIncludes(content, "jiji network setup");
});
