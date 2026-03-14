/**
 * Container network bridge health check and recovery.
 *
 * Detects when the podman/docker bridge interface is missing
 * (e.g., after a reboot or engine update) and recovers by
 * reloading all container networks.
 */

import type { Config } from "./types.ts";
import * as log from "./logger.ts";

/**
 * Check if the container network bridge exists and recover if missing.
 *
 * The jiji container network uses a bridge interface (e.g., podman1)
 * for container-to-host and cross-server traffic. If this bridge
 * disappears (reboot, engine update), containers lose all external
 * connectivity while appearing to still run.
 */
export async function reconcileNetworkBridge(
  config: Config,
): Promise<void> {
  const engine = config.engine;

  // Check if the jiji network exists
  const inspectCmd = new Deno.Command(engine, {
    args: ["network", "inspect", "jiji"],
    stdout: "piped",
    stderr: "piped",
  });

  const inspectOutput = await inspectCmd.output();
  if (!inspectOutput.success) {
    // No jiji network configured — nothing to reconcile
    return;
  }

  // Parse the network to find the expected bridge interface
  const stdout = new TextDecoder().decode(inspectOutput.stdout);
  let bridgeName: string | undefined;
  try {
    const networkData = JSON.parse(stdout);
    const network = networkData[0] ?? networkData;
    // Podman format
    bridgeName = network.network_interface ??
      // Docker format
      network.Options?.["com.docker.network.bridge.name"];
  } catch {
    log.warn("Failed to parse jiji network inspect output");
    return;
  }

  if (!bridgeName) {
    return;
  }

  // Check if the bridge interface actually exists
  const linkCmd = new Deno.Command("ip", {
    args: ["link", "show", bridgeName],
    stdout: "piped",
    stderr: "piped",
  });

  const linkOutput = await linkCmd.output();
  if (linkOutput.success) {
    // Bridge exists, nothing to do
    return;
  }

  // Bridge is missing — recover by reloading all container networks
  log.warn("Container network bridge missing, recovering", {
    bridge: bridgeName,
  });

  const reloadCmd = new Deno.Command(engine, {
    args: ["network", "reload", "--all"],
    stdout: "piped",
    stderr: "piped",
  });

  const reloadOutput = await reloadCmd.output();

  if (reloadOutput.success) {
    log.info("Container network bridge recovered", {
      bridge: bridgeName,
    });
  } else {
    const stderr = new TextDecoder().decode(reloadOutput.stderr);
    log.error("Failed to recover container network bridge", {
      bridge: bridgeName,
      error: stderr,
    });
  }
}
