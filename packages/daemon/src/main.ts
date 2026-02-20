/**
 * Jiji Daemon — Network reconciliation daemon
 *
 * Maintains WireGuard mesh health, container health tracking,
 * and cluster state via Corrosion CRDT database.
 */

import { parseConfig } from "./types.ts";
import * as log from "./logger.ts";
import { CorrosionClient } from "./corrosion_client.ts";
import { CorrosionCli } from "./corrosion_cli.ts";
import { reconcilePeers } from "./peer_reconciler.ts";
import { monitorPeerHealth } from "./peer_monitor.ts";
import { checkAllContainers } from "./container_health.ts";
import { garbageCollect } from "./garbage_collector.ts";
import { updatePublicIp } from "./ip_discovery.ts";
import { checkCorrosionHealth } from "./corrosion_health.ts";
import { detectSplitBrain } from "./split_brain.ts";
import { escapeSql, isValidContainerId } from "./validation.ts";
import type { ContainerRecord } from "./types.ts";

const BANNER = `
     _ _ _ _         _
    (_|_|_|_)       | |  __ _  ___ _ __ ___   ___  _ __
    | | | | |   ____| | / _\` |/ _ | '_ \` _ \\ / _ \\| '_ \\
   _| | | | |  |____| || (_| |  __| | | | | | (_) | | | |
  |___/_|_|_|        |_|\\__,_|\\___|_| |_| |_|\\___/|_| |_|
`;

async function main(): Promise<void> {
  console.log(BANNER);

  // Parse configuration
  let config;
  try {
    config = parseConfig();
  } catch (err) {
    console.error("Configuration error:", err);
    Deno.exit(1);
  }

  log.setServerId(config.serverId);

  log.info("Starting Jiji daemon", {
    server_id: config.serverId,
    engine: config.engine,
    interface: config.interfaceName,
    interval: config.loopInterval,
  });

  // Initialize components
  const client = new CorrosionClient(config.corrosionApi);
  const cli = new CorrosionCli(config.corrosionDir);

  let shuttingDown = false;
  let iteration = 0;

  // Graceful shutdown
  const shutdown = () => {
    log.info("Received shutdown signal, cleaning up...");
    shuttingDown = true;

    // Final heartbeat
    const now = Date.now();
    const escapedId = escapeSql(config.serverId);
    client.exec(
      `UPDATE servers SET last_seen = ${now} WHERE id = '${escapedId}';`,
    ).catch(() => {});

    log.info("Daemon shutdown complete");
    Deno.exit(0);
  };

  Deno.addSignalListener("SIGTERM", shutdown);
  Deno.addSignalListener("SIGINT", shutdown);

  // Main loop
  while (!shuttingDown) {
    iteration++;
    const iterationStart = Date.now();

    try {
      // 1. Update heartbeat
      const now = Date.now();
      const hbEscapedId = escapeSql(config.serverId);
      await client.exec(
        `UPDATE servers SET last_seen = ${now} WHERE id = '${hbEscapedId}';`,
      ).catch((err) =>
        log.error("Failed to update heartbeat", { error: String(err) })
      );

      // 2. Reconcile WireGuard peers
      await reconcilePeers(config, cli);

      // 3. Monitor peer health
      await monitorPeerHealth(config, cli);

      // 4. Sync container health
      await syncContainerHealth(config, client, cli);

      // 5. Garbage collection (every 10 iterations = 5 minutes)
      if (iteration % 10 === 0) {
        await garbageCollect(config, client, cli);
      }

      // 6. Update public IP (every 20 iterations = 10 minutes)
      if (iteration % 20 === 0) {
        await updatePublicIp(config, client, cli);
      }

      // 7. Check Corrosion health (every 20 iterations = 10 minutes)
      if (iteration % 20 === 0) {
        await checkCorrosionHealth(config, cli);
      }

      // 8. Detect cluster partition (every 20 iterations = 10 minutes)
      if (iteration % 20 === 0) {
        await detectSplitBrain(config, cli);
      }
    } catch (err) {
      log.error("Iteration error", {
        iteration,
        error: String(err),
      });
    }

    // Check iteration timing
    const iterationDuration = Math.floor((Date.now() - iterationStart) / 1000);
    if (iterationDuration > 15) {
      log.warn("Slow iteration", {
        iteration,
        duration_s: iterationDuration,
      });
    }

    // Log milestone every 100 iterations (50 minutes)
    if (iteration % 100 === 0) {
      log.info("Iteration milestone", { iteration });
    }

    // Sleep before next iteration
    await new Promise((resolve) =>
      setTimeout(resolve, config.loopInterval * 1000)
    );
  }
}

/**
 * Sync container health states with Corrosion.
 */
async function syncContainerHealth(
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
      healthPort: row[2] && row[2] !== "null" && row[2] !== "0"
        ? parseInt(row[2], 10)
        : null,
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
    } catch {
      // Continue with other containers
    }
  }

  if (changes > 0) {
    log.info("Container health sync complete", { changes });
  }
}

// Entry point
main().catch((error) => {
  console.error("Fatal error:", error);
  Deno.exit(1);
});
