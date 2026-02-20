/**
 * WireGuard operations via the `wg` command.
 */

import type { PeerState } from "./types.ts";
import * as log from "./logger.ts";

/**
 * Parse `wg show <interface> dump` output into typed peer states.
 * The first line is the interface itself, peers start from line 2.
 */
export async function showDump(
  interfaceName: string,
): Promise<PeerState[]> {
  const cmd = new Deno.Command("wg", {
    args: ["show", interfaceName, "dump"],
    stdout: "piped",
    stderr: "piped",
  });

  const output = await cmd.output();

  if (!output.success) {
    const stderr = new TextDecoder().decode(output.stderr);
    log.error("wg show dump failed", { stderr });
    return [];
  }

  const stdout = new TextDecoder().decode(output.stdout).trim();
  const lines = stdout.split("\n");

  // Skip first line (interface info), parse peer lines
  return lines.slice(1).filter((line) => line.trim() !== "").map((line) => {
    const fields = line.split("\t");
    return {
      publicKey: fields[0] ?? "",
      presharedKey: fields[1] ?? "",
      endpoint: fields[2] ?? "",
      allowedIps: fields[3] ?? "",
      latestHandshake: parseInt(fields[4] ?? "0", 10),
      transferRx: parseInt(fields[5] ?? "0", 10),
      transferTx: parseInt(fields[6] ?? "0", 10),
      persistentKeepalive: fields[7] ?? "off",
    };
  });
}

/**
 * Add or update a WireGuard peer.
 */
export async function setPeer(
  interfaceName: string,
  config: {
    publicKey: string;
    allowedIps: string;
    endpoint: string;
    persistentKeepalive?: number;
  },
): Promise<void> {
  const args = [
    "set",
    interfaceName,
    "peer",
    config.publicKey,
    "allowed-ips",
    config.allowedIps,
    "endpoint",
    config.endpoint,
  ];

  if (config.persistentKeepalive !== undefined) {
    args.push("persistent-keepalive", String(config.persistentKeepalive));
  }

  const cmd = new Deno.Command("wg", {
    args,
    stdout: "piped",
    stderr: "piped",
  });

  const output = await cmd.output();

  if (!output.success) {
    const stderr = new TextDecoder().decode(output.stderr);
    throw new Error(`Failed to set peer ${config.publicKey}: ${stderr}`);
  }
}

/**
 * Remove a WireGuard peer.
 */
export async function removePeer(
  interfaceName: string,
  publicKey: string,
): Promise<void> {
  const cmd = new Deno.Command("wg", {
    args: ["set", interfaceName, "peer", publicKey, "remove"],
    stdout: "piped",
    stderr: "piped",
  });

  const output = await cmd.output();

  if (!output.success) {
    const stderr = new TextDecoder().decode(output.stderr);
    throw new Error(`Failed to remove peer ${publicKey}: ${stderr}`);
  }
}

/**
 * Update only the endpoint for an existing peer.
 */
export async function updateEndpoint(
  interfaceName: string,
  publicKey: string,
  endpoint: string,
): Promise<void> {
  const cmd = new Deno.Command("wg", {
    args: [
      "set",
      interfaceName,
      "peer",
      publicKey,
      "endpoint",
      endpoint,
    ],
    stdout: "piped",
    stderr: "piped",
  });

  const output = await cmd.output();

  if (!output.success) {
    const stderr = new TextDecoder().decode(output.stderr);
    throw new Error(
      `Failed to update endpoint for ${publicKey}: ${stderr}`,
    );
  }
}
