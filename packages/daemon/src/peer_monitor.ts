/**
 * Monitor WireGuard peer health via handshake timestamps.
 *
 * Rotates endpoints when a peer's last handshake exceeds the threshold.
 */

import type { Config } from "./types.ts";
import { PEER_DOWN_THRESHOLD } from "./types.ts";
import type { CorrosionCli } from "./corrosion_cli.ts";
import * as wg from "./wireguard.ts";
import { parseEndpoints } from "./peer_reconciler.ts";
import { isValidEndpoint, isValidWireGuardKey, sql } from "./validation.ts";
import * as log from "./logger.ts";

/**
 * Monitor all peers and rotate endpoints for unresponsive ones.
 */
export async function monitorPeerHealth(
  config: Config,
  cli: CorrosionCli,
): Promise<void> {
  const peers = await wg.showDump(config.interfaceName);
  const now = Math.floor(Date.now() / 1000);

  for (const peer of peers) {
    if (peer.latestHandshake === 0) continue;

    const handshakeAge = now - peer.latestHandshake;

    if (handshakeAge > PEER_DOWN_THRESHOLD) {
      log.warn("Peer is down", {
        pubkey: peer.publicKey,
        handshake_age_s: handshakeAge,
      });

      await tryRotateEndpoint(config, cli, peer.publicKey, peer.endpoint);
    }
  }
}

/**
 * Try to rotate a peer's endpoint to the next available one.
 */
async function tryRotateEndpoint(
  config: Config,
  cli: CorrosionCli,
  publicKey: string,
  currentEndpoint: string,
): Promise<void> {
  if (!isValidWireGuardKey(publicKey)) {
    log.warn("Skipping endpoint rotation for invalid pubkey format", {
      pubkey: publicKey,
    });
    return;
  }

  const rows = await cli.query(
    sql`SELECT endpoints FROM servers WHERE wireguard_pubkey = ${publicKey};`,
  );

  if (rows.length === 0 || !rows[0][0]) return;

  const endpoints = parseEndpoints(rows[0][0]);
  if (endpoints.length <= 1) return;

  // Find current endpoint index and rotate to next
  const currentIndex = endpoints.indexOf(currentEndpoint);
  const nextIndex = (currentIndex + 1) % endpoints.length;
  const nextEndpoint = endpoints[nextIndex];

  if (nextEndpoint === currentEndpoint) return;

  if (!isValidEndpoint(nextEndpoint)) {
    log.warn("Skipping invalid endpoint", { endpoint: nextEndpoint });
    return;
  }

  log.info("Rotating endpoint", {
    pubkey: publicKey,
    from: currentEndpoint,
    to: nextEndpoint,
  });

  try {
    await wg.updateEndpoint(config.interfaceName, publicKey, nextEndpoint);
  } catch (err) {
    log.error("Failed to update endpoint", { error: String(err) });
  }
}
