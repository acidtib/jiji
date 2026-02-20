/**
 * Cluster partition / split-brain detection.
 *
 * Monitors the ratio of active servers to total servers and
 * alerts when less than 50% are reachable.
 */

import type { Config } from "./types.ts";
import { ACTIVE_SERVER_THRESHOLD } from "./types.ts";
import type { CorrosionCli } from "./corrosion_cli.ts";
import * as log from "./logger.ts";

/** Flag indicating whether a split-brain condition is currently detected. */
let _splitBrainDetected = false;

/** Check whether garbage collection should be paused due to split-brain. */
export function isSplitBrainDetected(): boolean {
  return _splitBrainDetected;
}

/**
 * Check for cluster partition / split-brain scenario.
 */
export async function detectSplitBrain(
  _config: Config,
  cli: CorrosionCli,
): Promise<void> {
  log.info("Checking for cluster partition...");

  const totalStr = await cli.queryScalar("SELECT COUNT(*) FROM servers;");
  const totalServers = parseInt(totalStr ?? "0", 10);

  if (totalServers === 0) {
    log.warn("Cannot determine total server count");
    return;
  }

  const now = Date.now();
  const activeThreshold = now - ACTIVE_SERVER_THRESHOLD;

  const activeStr = await cli.queryScalar(
    `SELECT COUNT(*) FROM servers WHERE last_seen > ${activeThreshold};`,
  );
  const activeServers = parseInt(activeStr ?? "0", 10);

  const reachablePct = totalServers > 0
    ? Math.floor((activeServers * 100) / totalServers)
    : 0;

  log.info("Cluster health check", {
    active: activeServers,
    total: totalServers,
    percent: reachablePct,
  });

  // Alert if less than 50% reachable and we have more than 1 server
  if (totalServers > 1 && reachablePct < 50) {
    _splitBrainDetected = true;
    log.error("POTENTIAL SPLIT-BRAIN — garbage collection paused", {
      active: activeServers,
      total: totalServers,
      percent: reachablePct,
    });

    // Log unreachable servers for debugging
    const rows = await cli.query(
      `SELECT hostname FROM servers WHERE last_seen <= ${activeThreshold};`,
    );
    const unreachable = rows.map((r) => r[0]).filter(Boolean);
    if (unreachable.length > 0) {
      log.error("Unreachable servers", {
        servers: unreachable,
      });
    }
  } else {
    if (_splitBrainDetected) {
      log.info("Split-brain condition resolved, resuming garbage collection");
    }
    _splitBrainDetected = false;
  }
}
