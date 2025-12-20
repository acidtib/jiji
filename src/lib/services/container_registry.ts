/**
 * Container registry service for network integration
 *
 * Handles registering containers in Corrosion database after deployment
 * for service discovery via DNS.
 */

import type { SSHManager } from "../../utils/ssh.ts";
import type { ContainerRegistration } from "../../types/network.ts";
import {
  registerContainer,
  registerService,
  unregisterContainer,
} from "../network/corrosion.ts";
import {
  registerContainerHostname,
  triggerHostsUpdate,
  unregisterContainerHostname,
} from "../network/dns.ts";
import { log } from "../../utils/logger.ts";
import type { Configuration } from "../configuration.ts";

/**
 * Get container IP address from Docker/Podman
 *
 * @param ssh - SSH connection to the server
 * @param containerId - Container ID or name
 * @param engine - Container engine (docker or podman)
 * @returns Container IP address or null if not found
 */
export async function getContainerIp(
  ssh: SSHManager,
  containerId: string,
  engine: "docker" | "podman",
): Promise<string | null> {
  const inspectCmd =
    `${engine} inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' ${containerId}`;

  const result = await ssh.executeCommand(inspectCmd);

  if (result.code !== 0) {
    log.warn(
      `Failed to get container IP for ${containerId}: ${result.stderr}`,
      "container",
    );
    return null;
  }

  const ip = result.stdout.trim();
  if (!ip || ip === "") {
    log.warn(`No IP address found for container ${containerId}`, "container");
    return null;
  }

  return ip;
}

/**
 * Get container ID by name
 *
 * @param ssh - SSH connection to the server
 * @param containerName - Container name
 * @param engine - Container engine (docker or podman)
 * @returns Container ID or null if not found
 */
export async function getContainerIdByName(
  ssh: SSHManager,
  containerName: string,
  engine: "docker" | "podman",
): Promise<string | null> {
  const psCmd = `${engine} ps -q -f name=${containerName}`;

  const result = await ssh.executeCommand(psCmd);

  if (result.code !== 0) {
    return null;
  }

  const id = result.stdout.trim();
  return id || null;
}

/**
 * Register a container in the network
 *
 * This function should be called after a container is successfully deployed.
 * It registers the container in Corrosion for service discovery.
 *
 * @param ssh - SSH connection to the server
 * @param serviceName - Service name (e.g., "api", "database")
 * @param projectName - Project name from config
 * @param serverId - Server ID from network topology
 * @param containerId - Container ID
 * @param engine - Container engine (docker or podman)
 * @returns True if registration was successful
 */
export async function registerContainerInNetwork(
  ssh: SSHManager,
  serviceName: string,
  projectName: string,
  serverId: string,
  containerId: string,
  engine: "docker" | "podman",
): Promise<boolean> {
  try {
    // Get container IP
    const ip = await getContainerIp(ssh, containerId, engine);
    if (!ip) {
      log.warn(
        `Cannot register container ${containerId} - no IP address`,
        "network",
      );
      return false;
    }

    // Register service first (idempotent)
    await registerService(ssh, {
      name: serviceName,
      project: projectName,
    });

    // Register container
    const registration: ContainerRegistration = {
      id: containerId,
      service: serviceName,
      serverId,
      ip,
      healthy: true,
      startedAt: Date.now(),
    };

    await registerContainer(ssh, registration);

    // Register container hostname in system DNS for immediate resolution
    try {
      await registerContainerHostname(
        ssh,
        serviceName,
        projectName,
        ip,
        containerId,
      );

      // CoreDNS will handle all resolution via project-service.jiji format
    } catch (error) {
      log.warn(`Failed to register container hostname: ${error}`, "network");
    }

    // Trigger immediate DNS hosts update
    try {
      await triggerHostsUpdate(ssh);
      log.debug(
        "Triggered DNS hosts update after container registration",
        "network",
      );
    } catch (error) {
      log.warn(`Failed to trigger DNS update: ${error}`, "network");
    }

    log.success(
      `Registered container ${serviceName} (${ip}) in network`,
      "network",
    );
    return true;
  } catch (error) {
    log.error(
      `Failed to register container ${containerId} in network: ${error}`,
      "network",
    );
    return false;
  }
}

/**
 * Unregister a container from the network
 *
 * This function should be called when a container is removed.
 *
 * @param ssh - SSH connection to the server
 * @param containerId - Container ID
 * @returns True if unregistration was successful
 */
export async function unregisterContainerFromNetwork(
  ssh: SSHManager,
  containerId: string,
  serviceName?: string,
  projectName?: string,
): Promise<boolean> {
  try {
    await unregisterContainer(ssh, containerId);

    // Unregister container hostname from system DNS
    if (serviceName) {
      try {
        await unregisterContainerHostname(
          ssh,
          serviceName,
          projectName,
          containerId,
        );
      } catch (error) {
        log.warn(
          `Failed to unregister container hostname: ${error}`,
          "network",
        );
      }
    }

    // Trigger immediate DNS hosts update after unregistration
    try {
      await triggerHostsUpdate(ssh);
      log.debug(
        "Triggered DNS hosts update after container unregistration",
        "network",
      );
    } catch (error) {
      log.warn(`Failed to trigger DNS update: ${error}`, "network");
    }

    log.success(
      `Unregistered container ${containerId} from network`,
      "network",
    );
    return true;
  } catch (error) {
    log.error(
      `Failed to unregister container ${containerId} from network: ${error}`,
      "network",
    );
    return false;
  }
}

/**
 * Update container health status in the network
 *
 * @param ssh - SSH connection to the server
 * @param containerId - Container ID
 * @param healthy - Whether the container is healthy
 * @returns True if update was successful
 */
export async function updateContainerHealth(
  ssh: SSHManager,
  containerId: string,
  healthy: boolean,
): Promise<boolean> {
  try {
    // Update health status in Corrosion
    const sql = `UPDATE containers SET healthy = ${
      healthy ? 1 : 0
    } WHERE id = '${containerId}';`;

    const result = await ssh.executeCommand(
      `/opt/jiji/corrosion/corrosion exec "${sql}"`,
    );

    if (result.code !== 0) {
      throw new Error(`Failed to update health: ${result.stderr}`);
    }

    log.debug(
      `Updated container ${containerId} health: ${healthy}`,
      "network",
    );
    return true;
  } catch (error) {
    log.error(
      `Failed to update container health for ${containerId}: ${error}`,
      "network",
    );
    return false;
  }
}

/**
 * Get all containers for a service from the network
 *
 * @param ssh - SSH connection to any server (Corrosion is distributed)
 * @param serviceName - Service name
 * @returns Array of container IPs
 */
export async function getServiceContainers(
  ssh: SSHManager,
  serviceName: string,
): Promise<string[]> {
  try {
    const sql =
      `SELECT ip FROM containers WHERE service = '${serviceName}' AND healthy = 1;`;

    const result = await ssh.executeCommand(
      `/opt/jiji/corrosion/corrosion exec "${sql}"`,
    );

    if (result.code !== 0) {
      throw new Error(`Failed to query containers: ${result.stderr}`);
    }

    return result.stdout.trim().split("\n").filter((ip: string) =>
      ip.length > 0
    );
  } catch (error) {
    log.error(
      `Failed to get containers for service ${serviceName}: ${error}`,
      "network",
    );
    return [];
  }
}

/**
 * Clean up all container references for a service
 *
 * This function removes containers from both Corrosion database and system DNS.
 * Useful when stopping or redeploying services.
 *
 * @param ssh - SSH connection to the server
 * @param serviceName - Service name
 * @param engine - Container engine (docker or podman)
 * @returns Number of containers cleaned up
 */
export async function cleanupServiceContainers(
  ssh: SSHManager,
  serviceName: string,
  engine: "docker" | "podman",
  projectName: string,
): Promise<number> {
  let cleanedCount = 0;

  try {
    // Get all containers for this service from Corrosion
    const sql = `SELECT id FROM containers WHERE service = '${serviceName}';`;

    const result = await ssh.executeCommand(
      `/opt/jiji/corrosion/corrosion exec --config /opt/jiji/corrosion/config.toml "${sql}"`,
    );

    if (result.code === 0) {
      const containerIds = result.stdout.trim().split("\n").filter((
        id: string,
      ) => id.length > 0);

      for (const containerId of containerIds) {
        try {
          // Check if container still exists in the container engine
          const inspectResult = await ssh.executeCommand(
            `${engine} inspect ${containerId} >/dev/null 2>&1`,
          );

          if (inspectResult.code !== 0) {
            // Container no longer exists, clean up from network
            await unregisterContainerFromNetwork(
              ssh,
              containerId,
              serviceName,
              projectName,
            );
            cleanedCount++;
            log.debug(
              `Cleaned up stale container reference: ${containerId}`,
              "network",
            );
          }
        } catch (error) {
          log.warn(
            `Failed to clean up container ${containerId}: ${error}`,
            "network",
          );
        }
      }
    }

    // Also clean up any remaining DNS entries for this service
    try {
      await unregisterContainerHostname(ssh, serviceName, projectName);
      log.debug(
        `Cleaned up DNS entries for service: ${serviceName}`,
        "network",
      );
    } catch (error) {
      log.warn(
        `Failed to clean up DNS entries for ${serviceName}: ${error}`,
        "network",
      );
    }

    // Trigger DNS update after cleanup
    try {
      await triggerHostsUpdate(ssh);
    } catch (error) {
      log.warn(
        `Failed to trigger DNS update after cleanup: ${error}`,
        "network",
      );
    }

    if (cleanedCount > 0) {
      log.success(
        `Cleaned up ${cleanedCount} stale container references for ${serviceName}`,
        "network",
      );
    }

    return cleanedCount;
  } catch (error) {
    log.error(
      `Failed to clean up service containers for ${serviceName}: ${error}`,
      "network",
    );
    return 0;
  }
}

/**
 * Register container across all servers in the cluster for DNS resolution
 *
 * This ensures that all servers know about containers on all other servers,
 * enabling proper DNS resolution for cross-server container communication.
 */
export async function registerContainerClusterWide(
  allSshManagers: SSHManager[],
  serviceName: string,
  projectName: string,
  serverId: string,
  containerId: string,
  containerIp: string,
  startedAt: number,
): Promise<void> {
  const registration: ContainerRegistration = {
    id: containerId,
    service: serviceName,
    serverId,
    ip: containerIp,
    healthy: true,
    startedAt,
  };

  // Register this container on all servers in the cluster
  for (const ssh of allSshManagers) {
    try {
      // Register service first (idempotent)
      await registerService(ssh, {
        name: serviceName,
        project: projectName,
      });

      // Register container
      await registerContainer(ssh, registration);

      log.debug(
        `Registered container ${containerId} on server ${ssh.getHost()}`,
        "network",
      );
    } catch (error) {
      log.warn(
        `Failed to register container ${containerId} on server ${ssh.getHost()}: ${error}`,
        "network",
      );
    }
  }
}
