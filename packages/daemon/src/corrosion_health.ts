/**
 * Corrosion service health monitoring.
 *
 * Checks that the Corrosion service is running, the database is responsive,
 * and heartbeat is being recorded.
 */

import type { Config } from "./types.ts";
import { HEARTBEAT_STALE_THRESHOLD } from "./types.ts";
import type { CorrosionCli } from "./corrosion_cli.ts";
import { sql } from "./validation.ts";
import * as log from "./logger.ts";

/**
 * Check if a systemd service is active.
 */
async function isServiceActive(serviceName: string): Promise<boolean> {
  const cmd = new Deno.Command("systemctl", {
    args: ["is-active", "--quiet", serviceName],
    stdout: "null",
    stderr: "null",
  });

  const output = await cmd.output();
  return output.success;
}

/**
 * Restart a systemd service.
 */
async function restartService(serviceName: string): Promise<boolean> {
  const cmd = new Deno.Command("systemctl", {
    args: ["restart", serviceName],
    stdout: "null",
    stderr: "null",
  });

  const output = await cmd.output();
  return output.success;
}

/**
 * Check Corrosion service health.
 */
export async function checkCorrosionHealth(
  config: Config,
  cli: CorrosionCli,
): Promise<void> {
  log.info("Checking Corrosion health...");

  // Check if service is running
  if (!await isServiceActive("jiji-corrosion")) {
    log.error("Corrosion service is not running!", {
      action: "restart_attempted",
    });

    if (await restartService("jiji-corrosion")) {
      log.info("Corrosion service restarted successfully");
      // Give it time to start
      await new Promise((resolve) => setTimeout(resolve, 5000));
    } else {
      log.error("Failed to restart Corrosion service");
      return;
    }
  }

  // Test database connectivity
  const testResult = await cli.queryScalar("SELECT 1;");
  if (testResult === null) {
    log.error("Corrosion database query failed");
    return;
  }

  // Check heartbeat freshness
  const lastSeen = await cli.queryScalar(
    sql`SELECT last_seen FROM servers WHERE id = ${config.serverId};`,
  );

  if (lastSeen) {
    const age = Date.now() - parseInt(lastSeen, 10);
    if (age > HEARTBEAT_STALE_THRESHOLD) {
      log.warn("Heartbeat appears stale", { age_ms: age });
    }
  }

  log.info("Corrosion health check passed");
}
