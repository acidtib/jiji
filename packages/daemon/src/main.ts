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
import { syncContainerHealth } from "./container_sync.ts";
import { garbageCollect } from "./garbage_collector.ts";
import { updatePublicIp } from "./ip_discovery.ts";
import { checkCorrosionHealth } from "./corrosion_health.ts";
import { detectSplitBrain } from "./split_brain.ts";
import { escapeSql } from "./validation.ts";

/** Run garbage collection every 10 iterations (~5 min at default 30s interval) */
const GC_INTERVAL = 10;
/** Update public IP every 20 iterations (~10 min) */
const IP_UPDATE_INTERVAL = 20;
/** Check Corrosion health every 20 iterations (~10 min) */
const CORROSION_HEALTH_INTERVAL = 20;
/** Detect split-brain every 20 iterations (~10 min) */
const SPLIT_BRAIN_INTERVAL = 20;
/** Log milestone every 100 iterations (~50 min) */
const MILESTONE_INTERVAL = 100;
/** Warn if a single iteration takes longer than 15 seconds */
const SLOW_ITERATION_THRESHOLD_S = 15;

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
      if (iteration % GC_INTERVAL === 0) {
        await garbageCollect(config, client, cli);
      }

      // 6. Update public IP (every 20 iterations = 10 minutes)
      if (iteration % IP_UPDATE_INTERVAL === 0) {
        await updatePublicIp(config, client, cli);
      }

      // 7. Check Corrosion health (every 20 iterations = 10 minutes)
      if (iteration % CORROSION_HEALTH_INTERVAL === 0) {
        await checkCorrosionHealth(config, cli);
      }

      // 8. Detect cluster partition (every 20 iterations = 10 minutes)
      if (iteration % SPLIT_BRAIN_INTERVAL === 0) {
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
    if (iterationDuration > SLOW_ITERATION_THRESHOLD_S) {
      log.warn("Slow iteration", {
        iteration,
        duration_s: iterationDuration,
      });
    }

    // Log milestone every 100 iterations (50 minutes)
    if (iteration % MILESTONE_INTERVAL === 0) {
      log.info("Iteration milestone", { iteration });
    }

    // Sleep before next iteration
    await new Promise((resolve) =>
      setTimeout(resolve, config.loopInterval * 1000)
    );
  }
}

// Entry point
main().catch((error) => {
  console.error("Fatal error:", error);
  Deno.exit(1);
});
