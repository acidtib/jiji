/**
 * Network inspect command
 *
 * Look up detailed information about a specific container
 * in the Corrosion distributed database.
 */

import { Command } from "@cliffy/command";
import {
  cleanupSSHConnections,
  displayCommandHeader,
  setupCommandContext,
} from "../../utils/command_helpers.ts";
import { handleCommandError } from "../../utils/error_handler.ts";
import { queryContainerById } from "../../lib/network/corrosion.ts";
import { loadTopology } from "../../lib/network/topology.ts";
import { log } from "../../utils/logger.ts";
import type { GlobalOptions } from "../../types.ts";

/**
 * Format timestamp as relative time
 */
function formatRelativeTime(timestampMs: number): string {
  const now = Date.now();
  const diffSeconds = Math.floor((now - timestampMs) / 1000);

  if (diffSeconds < 60) {
    return `${diffSeconds}s ago`;
  } else if (diffSeconds < 3600) {
    const mins = Math.floor(diffSeconds / 60);
    return `${mins}m ago`;
  } else if (diffSeconds < 86400) {
    const hours = Math.floor(diffSeconds / 3600);
    const mins = Math.floor((diffSeconds % 3600) / 60);
    return mins > 0 ? `${hours}h ${mins}m ago` : `${hours}h ago`;
  } else {
    const days = Math.floor(diffSeconds / 86400);
    const hours = Math.floor((diffSeconds % 86400) / 3600);
    return hours > 0 ? `${days}d ${hours}h ago` : `${days}d ago`;
  }
}

export const inspectCommand = new Command()
  .description("Inspect a specific container in the network database")
  .option(
    "--container <id:string>",
    "Container ID to look up (partial match supported)",
    { required: true },
  )
  .action(async (options) => {
    const globalOptions = options as unknown as GlobalOptions;
    let ctx: Awaited<ReturnType<typeof setupCommandContext>> | undefined;

    try {
      // Setup command context (load config and establish SSH connections)
      ctx = await setupCommandContext(globalOptions);
      const { config, sshManagers } = ctx;

      // Display standardized command header
      displayCommandHeader("Network Container Inspect:", config, sshManagers);

      // Try to load topology from any available server
      let topology = null;
      for (const ssh of sshManagers) {
        try {
          topology = await loadTopology(ssh);
          if (topology) break;
        } catch {
          continue;
        }
      }

      if (!topology) {
        console.log();
        log.say("No network cluster found", 1);
        log.say(
          "Run 'jiji server init' with network.enabled: true to set up private networking",
          1,
        );
        return;
      }

      if (topology.discovery !== "corrosion") {
        log.say("Container inspection requires Corrosion discovery mode", 1);
        return;
      }

      // Use first available SSH connection (Corrosion is distributed)
      const ssh = sshManagers[0];

      // Query container by ID
      const container = await queryContainerById(ssh, options.container);

      if (!container) {
        log.error(`Container not found: ${options.container}`, "network");
        log.say(
          "Try using a different ID prefix or run 'jiji network dns' to see all containers",
          1,
        );
        Deno.exit(1);
      }

      // Display container details
      log.section(`Container: ${container.id}`);
      console.log();

      const status = container.healthStatus;
      const statusColor = container.healthStatus === "healthy"
        ? "\x1b[32m"
        : "\x1b[31m";
      const resetColor = "\x1b[0m";

      // Build DNS names
      const baseDns =
        `${config.project}-${container.service}.${topology.serviceDomain}`;
      const instanceDns = container.instanceId
        ? `${config.project}-${container.service}-${container.instanceId}.${topology.serviceDomain}`
        : null;

      log.say(`├── Service: ${container.service}`, 1);
      log.say(`├── Server: ${container.serverHostname}`, 1);
      log.say(`├── Server ID: ${container.serverId}`, 1);
      log.say(`├── IP: ${container.ip}`, 1);
      log.say(`├── Status: ${statusColor}${status}${resetColor}`, 1);
      log.say(`├── Started: ${formatRelativeTime(container.startedAt)}`, 1);

      if (container.instanceId) {
        log.say(`├── Instance ID: ${container.instanceId}`, 1);
      }

      log.say(`└── DNS Names:`, 1);
      if (instanceDns) {
        log.say(`    ├── ${baseDns}`, 1);
        log.say(`    └── ${instanceDns}`, 1);
      } else {
        log.say(`    └── ${baseDns}`, 1);
      }

      console.log();
    } catch (error) {
      await handleCommandError(error, {
        operation: "Network container inspect",
        component: "network",
        sshManagers: ctx?.sshManagers,
        projectName: ctx?.config?.project,
        targetHosts: ctx?.targetHosts,
      });
    } finally {
      if (ctx?.sshManagers) {
        cleanupSSHConnections(ctx.sshManagers);
      }
    }
  });
