/**
 * Container network bridge health check and recovery.
 *
 * Detects when the podman/docker bridge interface is missing
 * (e.g., after a reboot or engine update) and recovers it.
 *
 * Strategy:
 *   1. `<engine> network reload --all` — the documented fix.
 *   2. If reload returns success but the bridge is still missing (we hit
 *      this in the field on some nodes), fall back to remove + recreate
 *      with the same subnet/gateway, but only when no containers are
 *      attached. With attached containers, refuse and log them — `rm`
 *      would either fail or silently disconnect workloads.
 */

import type { Config } from "./types.ts";
import * as log from "./logger.ts";

interface NetworkInfo {
  bridgeName?: string;
  subnet?: string;
  gateway?: string;
}

const NETWORK_NAME = "jiji";

export async function reconcileNetworkBridge(
  config: Config,
): Promise<void> {
  const engine = config.engine;

  const info = await inspectNetwork(engine);
  if (!info.bridgeName) {
    // No jiji network configured — nothing to reconcile.
    return;
  }

  if (await bridgeExists(info.bridgeName)) {
    return;
  }

  log.warn("Container network bridge missing, recovering", {
    bridge: info.bridgeName,
  });

  // Step 1: try reload.
  const reload = await runCmd(engine, ["network", "reload", "--all"]);
  if (!reload.success) {
    log.warn("network reload --all failed (continuing to fallback)", {
      stderr: reload.stderr.trim(),
    });
  }

  if (await bridgeExists(info.bridgeName)) {
    log.info("Container network bridge recovered via reload", {
      bridge: info.bridgeName,
    });
    return;
  }

  // Step 2: check for attached containers before destructive recreate.
  if (!info.subnet) {
    log.error(
      "Cannot recreate network: subnet not found in inspect output. Investigate manually.",
      { bridge: info.bridgeName },
    );
    return;
  }

  // Docker networks rely on engine-specific options (bridge name,
  // trusted_host_interfaces) that we can't always round-trip from inspect.
  // Re-run `jiji network setup` from the CLI for docker clusters — it has
  // the full option set in scope.
  if (engine === "docker") {
    log.error(
      "Bridge still missing after reload on docker. Re-run `jiji network setup` to recreate the network with the correct engine options.",
      { bridge: info.bridgeName },
    );
    return;
  }

  const attached = await listAttachedContainers(engine);
  if (attached.length > 0) {
    log.error(
      "Bridge still missing after reload, and containers are attached. Refusing to recreate the network (would disconnect workloads).",
      {
        bridge: info.bridgeName,
        attached_containers: attached,
      },
    );
    return;
  }

  // Step 3: rm + create with the existing subnet/gateway.
  const rm = await runCmd(engine, ["network", "rm", NETWORK_NAME]);
  if (!rm.success) {
    log.error("Failed to remove network for recreate", {
      bridge: info.bridgeName,
      stderr: rm.stderr.trim(),
    });
    return;
  }

  const createArgs = buildCreateArgs(engine, info.subnet, info.gateway);
  const create = await runCmd(engine, createArgs);
  if (!create.success) {
    log.error("Failed to recreate network", {
      bridge: info.bridgeName,
      stderr: create.stderr.trim(),
    });
    return;
  }

  if (await bridgeExists(info.bridgeName)) {
    log.info("Container network bridge recovered via recreate", {
      bridge: info.bridgeName,
      subnet: info.subnet,
    });
  } else {
    log.error(
      "Network recreated but bridge is still missing. The kernel may be unable to create the bridge (check journalctl for podman/netavark errors).",
      { bridge: info.bridgeName },
    );
  }
}

async function inspectNetwork(
  engine: "docker" | "podman",
): Promise<NetworkInfo> {
  const result = await runCmd(engine, ["network", "inspect", NETWORK_NAME]);
  if (!result.success) {
    return {};
  }

  try {
    const data = JSON.parse(result.stdout);
    const network = data[0] ?? data;

    const bridgeName: string | undefined = network.network_interface ??
      network.Options?.["com.docker.network.bridge.name"];

    let subnet: string | undefined;
    let gateway: string | undefined;
    // Podman (modern) format
    if (network.subnets && network.subnets[0]) {
      subnet = network.subnets[0].subnet;
      gateway = network.subnets[0].gateway;
    } else if (
      network.IPAM && network.IPAM.Config && network.IPAM.Config[0]
    ) {
      // Docker format
      subnet = network.IPAM.Config[0].Subnet;
      gateway = network.IPAM.Config[0].Gateway;
    }

    return { bridgeName, subnet, gateway };
  } catch {
    log.warn("Failed to parse jiji network inspect output");
    return {};
  }
}

async function bridgeExists(bridgeName: string): Promise<boolean> {
  const result = await runCmd("ip", ["link", "show", bridgeName]);
  return result.success;
}

async function listAttachedContainers(
  engine: "docker" | "podman",
): Promise<string[]> {
  const result = await runCmd(engine, [
    "ps",
    "-a",
    "--filter",
    `network=${NETWORK_NAME}`,
    "--format",
    "{{.Names}}",
  ]);
  if (!result.success) return [];
  return result.stdout.trim().split("\n").filter((n) => n.length > 0);
}

function buildCreateArgs(
  _engine: "docker" | "podman",
  subnet: string,
  gateway?: string,
): string[] {
  // Only podman gets here — docker is rejected before calling this helper
  // because we can't round-trip the docker bridge options from inspect.
  // Match the CLI's setup flag so daemon recreate doesn't drift from
  // what `jiji network setup` produces.
  const args = [
    "network",
    "create",
    NETWORK_NAME,
    `--subnet=${subnet}`,
    "--disable-dns",
  ];
  if (gateway) {
    args.push(`--gateway=${gateway}`);
  }
  return args;
}

async function runCmd(
  bin: string,
  args: string[],
): Promise<{ success: boolean; stdout: string; stderr: string }> {
  const cmd = new Deno.Command(bin, {
    args,
    stdout: "piped",
    stderr: "piped",
  });
  const output = await cmd.output();
  return {
    success: output.success,
    stdout: new TextDecoder().decode(output.stdout),
    stderr: new TextDecoder().decode(output.stderr),
  };
}
