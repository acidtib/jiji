/**
 * Network database command
 *
 * Direct database query interface for debugging and introspection
 * of the Corrosion distributed database.
 */

import { Command } from "@cliffy/command";
import {
  cleanupSSHConnections,
  displayCommandHeader,
  setupCommandContext,
} from "../../utils/command_helpers.ts";
import { handleCommandError } from "../../utils/error_handler.ts";
import { executeSql, getDbStats } from "../../lib/network/corrosion.ts";
import { loadTopology } from "../../lib/network/topology.ts";
import { log } from "../../utils/logger.ts";
import type { GlobalOptions } from "../../types.ts";

/**
 * Query subcommand - execute arbitrary SQL
 */
const queryCommand = new Command()
  .description("Execute a SQL query against the network database")
  .arguments("<sql:string>")
  .action(async (options, sql) => {
    const globalOptions = options as unknown as GlobalOptions;
    let ctx: Awaited<ReturnType<typeof setupCommandContext>> | undefined;

    try {
      // Setup command context (load config and establish SSH connections)
      ctx = await setupCommandContext(globalOptions);
      const { config, sshManagers } = ctx;

      // Display standardized command header
      displayCommandHeader("Network Database Query:", config, sshManagers);

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
        log.say("Database query requires Corrosion discovery mode", 1);
        return;
      }

      // Use first available SSH connection (Corrosion is distributed)
      const ssh = sshManagers[0];

      // Execute the SQL query
      log.section("Query:");
      log.say(sql, 1);
      console.log();

      log.section("Result:");
      const result = await executeSql(ssh, sql);

      if (result.trim()) {
        // Print raw output
        console.log(result);
      } else {
        log.say("(no results)", 1);
      }
    } catch (error) {
      await handleCommandError(error, {
        operation: "Network database query",
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

/**
 * Stats subcommand - show database statistics
 */
const statsCommand = new Command()
  .description("Show network database statistics")
  .action(async (options) => {
    const globalOptions = options as unknown as GlobalOptions;
    let ctx: Awaited<ReturnType<typeof setupCommandContext>> | undefined;

    try {
      // Setup command context (load config and establish SSH connections)
      ctx = await setupCommandContext(globalOptions);
      const { config, sshManagers } = ctx;

      // Display standardized command header
      displayCommandHeader("Network Database Stats:", config, sshManagers);

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
        log.say("Database stats require Corrosion discovery mode", 1);
        return;
      }

      // Use first available SSH connection (Corrosion is distributed)
      const ssh = sshManagers[0];

      // Get database statistics
      const stats = await getDbStats(ssh);

      log.section("Database Statistics:");
      console.log();

      log.say("Servers:", 1);
      log.say(`├── Total: ${stats.serverCount}`, 2);
      log.say(`└── Active (last 5m): ${stats.activeServerCount}`, 2);
      console.log();

      log.say("Containers:", 1);
      log.say(`├── Total: ${stats.containerCount}`, 2);
      log.say(`├── Healthy: ${stats.healthyContainerCount}`, 2);
      log.say(`└── Unhealthy: ${stats.unhealthyContainerCount}`, 2);
      console.log();

      log.say("Services:", 1);
      log.say(`└── Registered: ${stats.serviceCount}`, 2);
      console.log();

      // Calculate health percentage
      if (stats.containerCount > 0) {
        const healthPct = Math.round(
          (stats.healthyContainerCount / stats.containerCount) * 100,
        );
        const statusColor = healthPct >= 90
          ? "\x1b[32m"
          : healthPct >= 50
          ? "\x1b[33m"
          : "\x1b[31m";
        const resetColor = "\x1b[0m";
        log.say(
          `Overall Health: ${statusColor}${healthPct}%${resetColor} (${stats.healthyContainerCount}/${stats.containerCount} containers healthy)`,
          0,
        );
      }
    } catch (error) {
      await handleCommandError(error, {
        operation: "Network database stats",
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

/**
 * Main db command with subcommands
 */
export const dbCommand = new Command()
  .description("Network database operations")
  .action(function () {
    this.showHelp();
  })
  .command("query", queryCommand)
  .command("stats", statsCommand);
