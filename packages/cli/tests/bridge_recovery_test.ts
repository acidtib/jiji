/**
 * Tests for the missing-bridge recovery path in `jiji network setup`.
 *
 * Scenario the helper exists for: `<engine> network inspect jiji` reports
 * the network exists, but `ip link show <bridge>` shows nothing — usually
 * after a reboot or container engine update. `network reload --all` is the
 * documented fix, but we hit a node in the field where reload returned
 * success without actually recreating the bridge (silent no-op). These
 * tests pin the three branches of the strengthened recovery flow.
 */

import { assert, assertEquals } from "@std/assert";
import { MockSSHManager } from "./mocks.ts";
import { recoverMissingBridge } from "../src/lib/network/setup.ts";

// deno-lint-ignore no-explicit-any
const asSsh = (mock: MockSSHManager): any => mock;

const ok = { success: true, stdout: "", stderr: "", code: 0 };
const fail = (stderr = "") => ({
  success: false,
  stdout: "",
  stderr,
  code: 1,
});

Deno.test("recoverMissingBridge - reload restores the bridge (fast path)", async () => {
  const mock = new MockSSHManager("test-host");

  // Reload succeeds.
  mock.addMockResponse("network reload --all", ok);
  // The single ip-link check that follows the reload reports success.
  mock.addMockResponse("ip link show podman1", ok);

  const result = await recoverMissingBridge(
    asSsh(mock),
    "podman",
    "jiji",
    "podman1",
    "10.210.129.0/24",
    "10.210.129.1",
  );

  assertEquals(result.recovered, true);

  // No destructive commands should have been issued — reload was enough.
  const commands = mock.getAllCommands();
  assert(
    !commands.some((c) => c.includes("network rm")),
    "must not run `network rm` when reload already fixed it",
  );
  assert(
    !commands.some((c) => c.includes("network create")),
    "must not run `network create` when reload already fixed it",
  );
});

Deno.test("recoverMissingBridge - refuses recreate when containers attached", async () => {
  const mock = new MockSSHManager("test-host");

  mock.addMockResponse("network reload --all", ok);
  // Bridge still missing after reload.
  mock.addMockResponse("ip link show podman1", fail());
  // Two containers are still using the network.
  mock.addMockResponse("ps -a --filter network=jiji", {
    success: true,
    stdout: "casa-redis\ncasa-postgres\n",
    stderr: "",
    code: 0,
  });

  const result = await recoverMissingBridge(
    asSsh(mock),
    "podman",
    "jiji",
    "podman1",
    "10.210.129.0/24",
    "10.210.129.1",
  );

  assertEquals(result.recovered, false);
  assertEquals(result.attachedContainers, ["casa-redis", "casa-postgres"]);
  assert(
    result.failureReason && result.failureReason.includes("attached"),
    `expected reason to mention attached containers, got: ${result.failureReason}`,
  );

  const commands = mock.getAllCommands();
  assert(
    !commands.some((c) => c.includes("network rm")),
    "must not run destructive `network rm` while containers are attached",
  );
});

Deno.test("recoverMissingBridge - rm + create when no containers attached", async () => {
  const mock = new MockSSHManager("test-host");

  mock.addMockResponse("network reload --all", ok);
  // Bridge missing after reload, then back after recreate.
  mock.addMockResponseSequence("ip link show podman1", [
    fail(), // first check, after reload
    ok, // final check, after recreate
  ]);
  // No containers attached.
  mock.addMockResponse("ps -a --filter network=jiji", {
    success: true,
    stdout: "",
    stderr: "",
    code: 0,
  });
  mock.addMockResponse("network rm jiji", ok);
  mock.addMockResponse("network create jiji", ok);
  // waitForNetworkReady polls `network inspect jiji` and checks subnet/gateway.
  mock.addMockResponse(
    "network inspect jiji",
    {
      success: true,
      stdout: JSON.stringify([{
        subnets: [
          { subnet: "10.210.129.0/24", gateway: "10.210.129.1" },
        ],
      }]),
      stderr: "",
      code: 0,
    },
  );

  const result = await recoverMissingBridge(
    asSsh(mock),
    "podman",
    "jiji",
    "podman1",
    "10.210.129.0/24",
    "10.210.129.1",
  );

  assertEquals(result.recovered, true);

  const commands = mock.getAllCommands();
  // Order matters: reload first, then rm, then create. Otherwise the helper
  // could surprise us by reordering steps and accidentally destroying state.
  const reloadIdx = commands.findIndex((c) => c.includes("network reload"));
  const rmIdx = commands.findIndex((c) => c.includes("network rm jiji"));
  const createIdx = commands.findIndex((c) =>
    c.includes("network create jiji")
  );
  assert(reloadIdx >= 0 && rmIdx > reloadIdx && createIdx > rmIdx);
});

Deno.test("recoverMissingBridge - reports recreate failure with stderr", async () => {
  const mock = new MockSSHManager("test-host");

  mock.addMockResponse("network reload --all", ok);
  mock.addMockResponse("ip link show podman1", fail());
  mock.addMockResponse("ps -a --filter network=jiji", {
    success: true,
    stdout: "",
    stderr: "",
    code: 0,
  });
  mock.addMockResponse("network rm jiji", ok);
  // Create fails with a stderr we should see in the failure reason.
  mock.addMockResponse("network create jiji", fail("bridge name in use"));

  const result = await recoverMissingBridge(
    asSsh(mock),
    "podman",
    "jiji",
    "podman1",
    "10.210.129.0/24",
    "10.210.129.1",
  );

  assertEquals(result.recovered, false);
  assert(
    result.failureReason && result.failureReason.includes("bridge name in use"),
    `expected reason to surface stderr, got: ${result.failureReason}`,
  );
});

Deno.test("recoverMissingBridge wiring replaces the silent reload-only path", async () => {
  // Belt-and-braces: ensure setup.ts uses the helper and doesn't accidentally
  // regress to the old `reload --all 2>/dev/null || true` swallow.
  const content = await Deno.readTextFile(
    "src/lib/network/setup.ts",
  );
  assert(
    content.includes("recoverMissingBridge("),
    "setup.ts must call recoverMissingBridge instead of inline reload",
  );
  assert(
    !content.includes("network reload --all 2>/dev/null || true"),
    "the silent `|| true` reload path must not return",
  );
});
