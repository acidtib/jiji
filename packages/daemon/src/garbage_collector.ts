/**
 * Cluster-wide garbage collection.
 *
 * Removes stale container records that have been unhealthy too long,
 * and cleans up containers from offline servers.
 */

import type { Config } from "./types.ts";
import {
  CLOCK_SKEW_MARGIN,
  OFFLINE_SERVER_THRESHOLD,
  STALE_CONTAINER_THRESHOLD,
} from "./types.ts";
import type { CorrosionClient } from "./corrosion_client.ts";
import type { CorrosionCli } from "./corrosion_cli.ts";
import {
  escapeSql,
  isValidContainerId,
  isValidServerId,
} from "./validation.ts";
import { isSplitBrainDetected } from "./split_brain.ts";
import * as log from "./logger.ts";

/**
 * Run garbage collection (called every 5 minutes).
 */
export async function garbageCollect(
  config: Config,
  client: CorrosionClient,
  cli: CorrosionCli,
): Promise<void> {
  if (isSplitBrainDetected()) {
    log.warn("Skipping garbage collection — split-brain detected");
    return;
  }

  log.info("Running cluster-wide container garbage collection...");

  let deleted = 0;

  // Delete containers that have been unhealthy for more than 3 minutes
  deleted += await deleteStaleContainers(client, cli);

  // Delete containers from offline servers (no heartbeat in 10 minutes)
  deleted += await deleteOfflineServerContainers(config, client, cli);

  if (deleted > 0) {
    log.info(
      "Garbage collection complete",
      { removed: deleted },
    );
  }
}

async function deleteStaleContainers(
  client: CorrosionClient,
  cli: CorrosionCli,
): Promise<number> {
  const now = Math.floor(Date.now() / 1000);
  const threshold = now - STALE_CONTAINER_THRESHOLD - CLOCK_SKEW_MARGIN;

  const rows = await cli.query(
    `SELECT id, service FROM containers WHERE health_status != 'healthy' AND (started_at / 1000) < ${threshold};`,
  );

  let deleted = 0;
  for (const [containerId, service] of rows) {
    if (!containerId) continue;

    if (!isValidContainerId(containerId)) {
      log.warn("Skipping container with invalid ID format", {
        container_id: containerId,
      });
      continue;
    }

    log.info("Deleting stale container", {
      container_id: containerId,
      service,
    });

    const escaped = escapeSql(containerId);
    const affected = await client.execGetRowsAffected(
      `DELETE FROM containers WHERE id = '${escaped}';`,
    );
    deleted += affected > 0 ? 1 : 0;
  }

  return deleted;
}

async function deleteOfflineServerContainers(
  config: Config,
  client: CorrosionClient,
  cli: CorrosionCli,
): Promise<number> {
  const now = Date.now();
  const threshold = now - OFFLINE_SERVER_THRESHOLD;

  const escapedServerId = escapeSql(config.serverId);
  const rows = await cli.query(
    `SELECT id FROM servers WHERE last_seen < ${threshold} AND id != '${escapedServerId}';`,
  );

  let deleted = 0;
  for (const [serverId] of rows) {
    if (!serverId) continue;

    if (!isValidServerId(serverId)) {
      log.warn("Skipping server with invalid ID format", {
        server_id: serverId,
      });
      continue;
    }

    log.warn("Server appears offline, cleaning up containers", {
      server_id: serverId,
    });

    const escaped = escapeSql(serverId);
    const affected = await client.execGetRowsAffected(
      `DELETE FROM containers WHERE server_id = '${escaped}';`,
    );
    deleted += affected;
  }

  return deleted;
}
