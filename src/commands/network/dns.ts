/**
 * Network DNS command
 *
 * Shows DNS records from the Corrosion distributed database,
 * grouped by service with container health status.
 */

import { Command } from "@cliffy/command";
import {
  cleanupSSHConnections,
  displayCommandHeader,
  setupCommandContext,
} from "../../utils/command_helpers.ts";
import { handleCommandError } from "../../utils/error_handler.ts";
import { queryAllContainersWithDetails } from "../../lib/network/corrosion.ts";
import { loadTopology } from "../../lib/network/topology.ts";
import { log } from "../../utils/logger.ts";
import type { GlobalOptions } from "../../types.ts";

export const dnsCommand = new Command()
  .description("Show DNS records from the network database")
  .action(async (options) => {
    const globalOptions = options as unknown as GlobalOptions;
    let ctx: Awaited<ReturnType<typeof setupCommandContext>> | undefined;

    try {
      // Setup command context (load config and establish SSH connections)
      ctx = await setupCommandContext(globalOptions);
      const { config, sshManagers, matchingServices } = ctx;

      // Display standardized command header
      displayCommandHeader("Network DNS Records:", config, sshManagers);

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
        log.say("DNS records require Corrosion discovery mode", 1);
        return;
      }

      // Use first available SSH connection (Corrosion is distributed)
      const ssh = sshManagers[0];

      // Get all containers with details
      const containers = await queryAllContainersWithDetails(ssh);

      if (containers.length === 0) {
        log.say("No containers registered in the network", 1);
        return;
      }

      // Filter by services if --services flag was provided
      let filteredContainers = containers;
      if (matchingServices && matchingServices.length > 0) {
        filteredContainers = containers.filter((c) =>
          matchingServices.includes(c.service)
        );

        if (filteredContainers.length === 0) {
          log.say(
            `No containers found for service(s): ${
              matchingServices.join(", ")
            }`,
            1,
          );
          return;
        }
      }

      // Group containers by service
      const byService = new Map<
        string,
        Array<{
          ip: string;
          serverHostname: string;
          healthy: boolean;
          instanceId?: string;
        }>
      >();

      for (const container of filteredContainers) {
        const key = container.service;
        if (!byService.has(key)) {
          byService.set(key, []);
        }
        byService.get(key)!.push({
          ip: container.ip,
          serverHostname: container.serverHostname,
          healthy: container.healthy,
          instanceId: container.instanceId,
        });
      }

      // Display DNS records grouped by service
      log.section("DNS Records:");

      const serviceNames = Array.from(byService.keys()).sort();
      for (const serviceName of serviceNames) {
        const records = byService.get(serviceName)!;

        // Build DNS name
        const dnsName =
          `${config.project}-${serviceName}.${topology.serviceDomain}`;
        const healthyCount = records.filter((r) => r.healthy).length;

        console.log();
        log.say(
          `${dnsName} (${healthyCount}/${records.length} healthy)`,
          1,
        );

        // Sort records: healthy first, then by server hostname
        records.sort((a, b) => {
          if (a.healthy !== b.healthy) {
            return a.healthy ? -1 : 1;
          }
          return a.serverHostname.localeCompare(b.serverHostname);
        });

        for (let i = 0; i < records.length; i++) {
          const record = records[i];
          const isLast = i === records.length - 1;
          const prefix = isLast ? "└──" : "├──";
          const status = record.healthy ? "healthy" : "unhealthy";
          const statusColor = record.healthy ? "\x1b[32m" : "\x1b[31m";
          const resetColor = "\x1b[0m";

          let line =
            `${prefix} ${record.ip} (${record.serverHostname}) - ${statusColor}${status}${resetColor}`;

          // Show instance-specific DNS if instanceId exists
          if (record.instanceId) {
            const instanceDns =
              `${config.project}-${serviceName}-${record.instanceId}.${topology.serviceDomain}`;
            line += ` [${instanceDns}]`;
          }

          log.say(line, 2);
        }
      }

      console.log();
      log.success(
        `Found ${filteredContainers.length} container(s) across ${byService.size} service(s)`,
        0,
      );
    } catch (error) {
      await handleCommandError(error, {
        operation: "Network DNS records",
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
