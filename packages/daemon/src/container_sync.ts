/**
 * Container health synchronization with Corrosion.
 *
 * Queries local containers from the CRDT database, runs health checks,
 * and writes updated status back via the Corrosion HTTP API.
 */

import type { parseConfig } from "./types.ts";
import type { CorrosionClient } from "./corrosion_client.ts";
import type { CorrosionCli } from "./corrosion_cli.ts";
import { escapeSql, isValidContainerId } from "./validation.ts";
import type { ContainerRecord } from "./types.ts";
import { checkAllContainers } from "./container_health.ts";
import * as log from "./logger.ts";

/**
 * Parse a health port string from Corrosion into a numeric port or null.
 * Handles missing values, "null" strings, zero, and non-numeric garbage.
 */
function parseHealthPort(value: string | undefined): number | null {
  if (!value || value === "null" || value === "0") return null;
  const port = parseInt(value, 10);
  return isNaN(port) ? null : port;
}

/**
 * Sync container health states with Corrosion.
 */
export async function syncContainerHealth(
  config: ReturnType<typeof parseConfig>,
  client: CorrosionClient,
  cli: CorrosionCli,
): Promise<void> {
  // Get containers from this server
  const syncEscapedId = escapeSql(config.serverId);
  const rows = await cli.query(
    `SELECT id, ip, health_port, health_status, consecutive_failures FROM containers WHERE server_id = '${syncEscapedId}';`,
  );

  if (rows.length === 0) return;

  const containers: ContainerRecord[] = rows
    .filter((r) => r[0]?.trim())
    .map((row) => ({
      id: row[0],
      ip: row[1],
      healthPort: parseHealthPort(row[2]),
      healthStatus: row[3] ?? "",
      consecutiveFailures: parseInt(row[4] ?? "0", 10),
      serverId: config.serverId,
    }));

  const results = await checkAllContainers(config.engine, containers);

  let changes = 0;
  for (const result of results) {
    if (!result.changed) continue;

    if (!isValidContainerId(result.containerId)) {
      log.warn("Skipping health update for container with invalid ID", {
        container_id: result.containerId,
      });
      continue;
    }

    const now = Date.now();
    const escapedCId = escapeSql(result.containerId);
    const escapedStatus = escapeSql(result.newStatus);
    try {
      await client.exec(
        `UPDATE containers SET health_status = '${escapedStatus}', last_health_check = ${now}, consecutive_failures = ${result.newFailures} WHERE id = '${escapedCId}';`,
      );
      changes++;
      log.info("Container health changed", {
        container_id: result.containerId,
        status: result.newStatus,
        failures: result.newFailures,
      });
    } catch (err) {
      log.warn("Failed to update container health in Corrosion", {
        container_id: result.containerId,
        error: String(err),
      });
    }
  }

  if (changes > 0) {
    log.info("Container health sync complete", { changes });
  }
}
