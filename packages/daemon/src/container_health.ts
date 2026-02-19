/**
 * Container health checking via TCP connection tests.
 *
 * Uses Deno.connect() for TCP checks — no bash dependency.
 * Runs health checks in parallel with Promise.allSettled().
 */

import type { ContainerRecord, HealthResult } from "./types.ts";
import * as log from "./logger.ts";

const TCP_TIMEOUT_MS = 2000;

/**
 * Perform a TCP health check on a single container port.
 * Returns true if the connection succeeds within the timeout.
 */
export async function checkTcpHealth(
  ip: string,
  port: number,
  timeoutMs: number = TCP_TIMEOUT_MS,
): Promise<boolean> {
  const abort = new AbortController();
  const timer = setTimeout(() => abort.abort(), timeoutMs);

  try {
    const conn = await Deno.connect({
      hostname: ip,
      port,
      signal: abort.signal,
    });
    conn.close();
    return true;
  } catch {
    return false;
  } finally {
    clearTimeout(timer);
    abort.abort(); // Ensure any pending connect is cancelled
  }
}

/**
 * Check if a container process is running via the container engine.
 */
async function isContainerRunning(
  engine: "docker" | "podman",
  containerId: string,
): Promise<boolean> {
  const cmd = new Deno.Command(engine, {
    args: ["ps", "-q", "--filter", `id=${containerId}`],
    stdout: "piped",
    stderr: "piped",
  });

  const output = await cmd.output();
  if (!output.success) return false;

  const stdout = new TextDecoder().decode(output.stdout).trim();
  return stdout.length > 0;
}

/**
 * Check health of all containers in parallel.
 */
export async function checkAllContainers(
  engine: "docker" | "podman",
  containers: ContainerRecord[],
): Promise<HealthResult[]> {
  const results = await Promise.allSettled(
    containers.map((c) => checkSingleContainer(engine, c)),
  );

  return results.map((result, i) => {
    if (result.status === "fulfilled") {
      return result.value;
    }
    log.error("Health check error", {
      containerId: containers[i].id,
      error: String(result.reason),
    });
    return {
      containerId: containers[i].id,
      newStatus: containers[i].healthStatus,
      newFailures: containers[i].consecutiveFailures,
      changed: false,
    };
  });
}

/**
 * Check health of a single container.
 */
async function checkSingleContainer(
  engine: "docker" | "podman",
  container: ContainerRecord,
): Promise<HealthResult> {
  const { id, ip, healthPort, healthStatus, consecutiveFailures } = container;
  let newStatus = "";
  let newFailures = consecutiveFailures;

  // Check if container process is running
  const running = await isContainerRunning(engine, id);

  if (!running) {
    newStatus = "unhealthy";
    newFailures = consecutiveFailures + 1;
  } else if (healthPort === null || healthPort === 0) {
    // No health port configured — assume healthy if running
    newStatus = "healthy";
    newFailures = 0;
  } else {
    // TCP health check
    const healthy = await checkTcpHealth(ip, healthPort);
    if (healthy) {
      newStatus = "healthy";
      newFailures = 0;
    } else {
      newFailures = consecutiveFailures + 1;
      if (newFailures >= 3) {
        newStatus = "unhealthy";
      } else {
        newStatus = "degraded";
      }
    }
  }

  return {
    containerId: id,
    newStatus,
    newFailures,
    changed: newStatus !== healthStatus || newFailures !== consecutiveFailures,
  };
}
