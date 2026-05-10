/**
 * Regression tests for split-brain healing.
 *
 * `healSplitBrain` shells out to `wg`, so we can't meaningfully unit-test
 * its behaviour without mocking the WireGuard subprocess. Instead we
 * pin the invariants that make the heal actually heal:
 *
 *   - Heal exists, is exported, and lives in peer_cache.ts (so it uses
 *     the same cache file the boot hydrate writes).
 *   - Heal is wired into the main loop, gated by isSplitBrainDetected().
 *   - The wiring runs every iteration, not only on the SPLIT_BRAIN_INTERVAL
 *     boundary — the flag is sticky, and we want a ~30s heal cadence.
 *   - Heal sources endpoints from the cache, not from Corrosion, because
 *     Corrosion is the thing that's broken during a partition.
 *   - Heal respects PEER_DOWN_THRESHOLD so we don't flap endpoints on a
 *     peer that just hasn't handshaked yet.
 */

import { assert, assertStringIncludes } from "@std/assert";

Deno.test("healSplitBrain is exported from peer_cache.ts", async () => {
  const content = await Deno.readTextFile("src/peer_cache.ts");
  assertStringIncludes(content, "export async function healSplitBrain");
});

Deno.test("healSplitBrain uses cached endpoints, not Corrosion", async () => {
  const content = await Deno.readTextFile("src/peer_cache.ts");
  const fnStart = content.indexOf("export async function healSplitBrain");
  assert(fnStart >= 0);
  // Take a generous slice — heal body is around 80 lines.
  const fnSection = content.slice(fnStart, fnStart + 4000);

  // Must use the cache file (loadCache) as the source of truth.
  assertStringIncludes(fnSection, "loadCache(cachePath)");

  // Must rotate using cached endpoints array, not anything Corrosion-related.
  // Guards against a regression that reaches back into Corrosion.
  assert(
    !fnSection.includes("CorrosionCli") &&
      !fnSection.includes("CorrosionClient") &&
      !fnSection.includes("from \"./corrosion"),
    "healSplitBrain must not depend on Corrosion (it's the broken thing)",
  );
});

Deno.test("healSplitBrain respects PEER_DOWN_THRESHOLD", async () => {
  const content = await Deno.readTextFile("src/peer_cache.ts");
  const fnStart = content.indexOf("export async function healSplitBrain");
  const fnSection = content.slice(fnStart, fnStart + 4000);
  assertStringIncludes(fnSection, "PEER_DOWN_THRESHOLD");
});

Deno.test("main loop runs heal every iteration when flag is set", async () => {
  const content = await Deno.readTextFile("src/main.ts");
  // The detection call is interval-gated; the heal call must NOT be.
  const detectIdx = content.indexOf("detectSplitBrain(config, cli)");
  const healIdx = content.indexOf("healSplitBrain(");
  assert(detectIdx >= 0, "detectSplitBrain wiring missing");
  assert(healIdx >= 0, "healSplitBrain wiring missing in main loop");

  // The block immediately preceding healSplitBrain must check
  // isSplitBrainDetected(), not the interval modulus, so the heal runs
  // every loop tick instead of only at the detection boundary.
  const preHeal = content.slice(Math.max(0, healIdx - 200), healIdx);
  assertStringIncludes(preHeal, "isSplitBrainDetected()");
  assert(
    !preHeal.includes("% SPLIT_BRAIN_INTERVAL"),
    "heal must not be gated by SPLIT_BRAIN_INTERVAL (flag is sticky)",
  );
});

Deno.test("main loop imports both detection flag and heal", async () => {
  const content = await Deno.readTextFile("src/main.ts");
  assertStringIncludes(content, "isSplitBrainDetected");
  assertStringIncludes(content, "healSplitBrain");
});
