/**
 * Network garbage collection command
 *
 * Cleans up stale container records and containers from offline servers
 * in the Corrosion distributed database.
 */

import { Command } from "@cliffy/command";
import {
  cleanupSSHConnections,
  displayCommandHeader,
  setupCommandContext,
} from "../../utils/command_helpers.ts";
import { handleCommandError } from "../../utils/error_handler.ts";
import {
  deleteContainersByIds,
  deleteContainersByServer,
  queryOfflineServers,
  queryStaleContainers,
} from "../../lib/network/corrosion.ts";
import { loadTopology } from "../../lib/network/topology.ts";
import { log } from "../../utils/logger.ts";
import type { GlobalOptions } from "../../types.ts";

/**
 * Format duration in human-readable format
 */
function formatDuration(seconds: number): string {
  if (seconds < 60) {
    return `${seconds}s`;
  } else if (seconds < 3600) {
    const mins = Math.floor(seconds / 60);
    const secs = seconds % 60;
    return secs > 0 ? `${mins}m ${secs}s` : `${mins}m`;
  } else {
    const hours = Math.floor(seconds / 3600);
    const mins = Math.floor((seconds % 3600) / 60);
    return mins > 0 ? `${hours}h ${mins}m` : `${hours}h`;
  }
}

export const gcCommand = new Command()
  .description("Garbage collect stale container and server records")
  .option(
    "--dry-run",
    "Show what would be deleted without executing (default if --force not specified)",
  )
  .option("--force", "Actually perform the deletions")
  .option(
    "--stale-threshold <seconds:number>",
    "Stale container threshold in seconds",
    { default: 180 },
  )
  .option(
    "--offline-threshold <seconds:number>",
    "Offline server threshold in seconds",
    { default: 600 },
  )
  .action(async (options) => {
    const globalOptions = options as unknown as GlobalOptions;
    let ctx: Awaited<ReturnType<typeof setupCommandContext>> | undefined;

    try {
      // Setup command context (load config and establish SSH connections)
      ctx = await setupCommandContext(globalOptions);
      const { config, sshManagers } = ctx;

      // Determine if this is a dry run (default unless --force is specified)
      const isDryRun = !options.force;
      const modeLabel = isDryRun ? " (DRY RUN)" : "";

      // Display standardized command header
      displayCommandHeader(
        `Network Garbage Collection${modeLabel}:`,
        config,
        sshManagers,
      );

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
        log.say("Garbage collection requires Corrosion discovery mode", 1);
        return;
      }

      // Use first available SSH connection (Corrosion is distributed)
      const ssh = sshManagers[0];
      let totalToDelete = 0;

      // Query stale containers
      log.section("Stale Containers:");
      const staleContainers = await queryStaleContainers(
        ssh,
        options.staleThreshold,
      );

      if (staleContainers.length === 0) {
        log.say("No stale containers found", 1);
      } else {
        log.say(
          `Found ${staleContainers.length} container(s) unhealthy > ${
            formatDuration(options.staleThreshold)
          }:`,
          1,
        );
        for (let i = 0; i < staleContainers.length; i++) {
          const container = staleContainers[i];
          const isLast = i === staleContainers.length - 1;
          const prefix = isLast ? "└──" : "├──";
          log.say(
            `${prefix} ${
              container.id.substring(0, 12)
            } (${container.service}) - unhealthy for ${
              formatDuration(container.unhealthySince)
            }`,
            2,
          );
        }
        totalToDelete += staleContainers.length;
      }

      // Query offline servers
      console.log();
      log.section("Offline Server Containers:");
      const offlineServers = await queryOfflineServers(
        ssh,
        options.offlineThreshold * 1000, // Convert to milliseconds
      );

      const offlineWithContainers = offlineServers.filter(
        (s) => s.containerCount > 0,
      );

      if (offlineWithContainers.length === 0) {
        log.say("No containers from offline servers found", 1);
      } else {
        log.say(
          `Found ${offlineWithContainers.length} offline server(s) with containers:`,
          1,
        );
        for (let i = 0; i < offlineWithContainers.length; i++) {
          const server = offlineWithContainers[i];
          const isLast = i === offlineWithContainers.length - 1;
          const prefix = isLast ? "└──" : "├──";
          const offlineFor = Math.floor((Date.now() - server.lastSeen) / 1000);
          log.say(
            `${prefix} ${server.hostname} (offline ${
              formatDuration(offlineFor)
            }) - ${server.containerCount} container(s) to remove`,
            2,
          );
          totalToDelete += server.containerCount;
        }
      }

      // Summary
      console.log();
      if (totalToDelete === 0) {
        log.success("No records to clean up", 0);
        return;
      }

      if (isDryRun) {
        log.warn(`Total: ${totalToDelete} record(s) would be deleted`, 0);
        log.say("Run with --force to actually delete these records", 1);
      } else {
        // Perform actual deletions
        log.section("Deleting Records:");

        let deleted = 0;

        // Delete stale containers
        if (staleContainers.length > 0) {
          const ids = staleContainers.map((c) => c.id);
          const count = await deleteContainersByIds(ssh, ids);
          deleted += count;
          log.say(`Deleted ${count} stale container record(s)`, 1);
        }

        // Delete containers from offline servers
        for (const server of offlineWithContainers) {
          const count = await deleteContainersByServer(ssh, server.id);
          deleted += count;
          log.say(
            `Deleted ${count} container(s) from offline server ${server.hostname}`,
            1,
          );
        }

        console.log();
        log.success(
          `Garbage collection complete: ${deleted} record(s) deleted`,
          0,
        );
      }
    } catch (error) {
      await handleCommandError(error, {
        operation: "Network garbage collection",
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
