/**
 * Public IP discovery and endpoint updates.
 *
 * Periodically checks the server's public IP and updates Corrosion
 * endpoints if it has changed.
 */

import type { Config } from "./types.ts";
import type { CorrosionClient } from "./corrosion_client.ts";
import type { CorrosionCli } from "./corrosion_cli.ts";
import { sql } from "./validation.ts";
import * as log from "./logger.ts";

const IP_SERVICES = [
  "https://api.ipify.org",
  "https://ipinfo.io/ip",
  "https://icanhazip.com",
];

const IP_REGEX = /^\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}$/;
const WIREGUARD_PORT = 31820;

/**
 * Discover the current public IP address.
 * Tries multiple services, returns first valid result.
 */
export async function discoverPublicIp(): Promise<string | null> {
  for (const service of IP_SERVICES) {
    try {
      const response = await fetch(service, {
        signal: AbortSignal.timeout(5000),
      });
      const text = (await response.text()).trim();

      if (IP_REGEX.test(text)) {
        return text;
      }
    } catch {
      // Try next service
    }
  }

  return null;
}

/**
 * Check for public IP changes and update Corrosion endpoints.
 */
export async function updatePublicIp(
  config: Config,
  client: CorrosionClient,
  cli: CorrosionCli,
): Promise<void> {
  log.info("Checking for public IP changes...");

  const newIp = await discoverPublicIp();
  if (!newIp) {
    log.warn("Could not discover public IP");
    return;
  }

  // Get current endpoints
  const current = await cli.queryScalar(
    sql`SELECT endpoints FROM servers WHERE id = ${config.serverId};`,
  );

  if (current && current.includes(newIp)) {
    return; // IP hasn't changed
  }

  log.info("Public IP changed, updating endpoints", { ip: newIp });

  const newEndpoint = `${newIp}:${WIREGUARD_PORT}`;
  const endpointsJson = JSON.stringify([newEndpoint]);

  try {
    await client.exec(
      sql`UPDATE servers SET endpoints = ${endpointsJson} WHERE id = ${config.serverId};`,
    );
  } catch (err) {
    log.error("Failed to update endpoints", { error: String(err) });
  }
}
