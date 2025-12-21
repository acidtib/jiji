/**
 * Network teardown command
 *
 * Tears down the private network by stopping services,
 * removing configurations, and cleaning up state.
 */

import { Command } from "@cliffy/command";
import {
  cleanupSSHConnections,
  setupCommandContext,
} from "../../utils/command_helpers.ts";
import { handleCommandError } from "../../utils/error_handler.ts";
import { loadTopology } from "../../lib/network/topology.ts";
import {
  bringDownWireGuardInterface,
  disableWireGuardService,
} from "../../lib/network/wireguard.ts";
import { stopCorrosionService } from "../../lib/network/corrosion.ts";
import { stopCoreDNSService } from "../../lib/network/dns.ts";
import { log, Logger } from "../../utils/logger.ts";
import { DEFAULT_MAX_PREFIX_LENGTH } from "../../constants.ts";
import type { GlobalOptions } from "../../types.ts";

export const teardownCommand = new Command()
  .description("Tear down private network")
  .action(async (options) => {
    const globalOptions = options as unknown as GlobalOptions;
    let ctx: Awaited<ReturnType<typeof setupCommandContext>> | undefined;

    try {
      await log.group("Network Teardown", async () => {
        // Set up command context
        ctx = await setupCommandContext(globalOptions);
        const { sshManagers, targetHosts } = ctx;

        // Try to load topology from Corrosion
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
          log.info("No network cluster found in Corrosion", "network");
          log.info("Nothing to tear down", "network");
          return;
        }

        log.warn(
          "This will tear down the private network on all servers",
          "network",
        );
        log.info(`Servers: ${topology.servers.length}`, "network");

        const serverLoggers = Logger.forServers(targetHosts, {
          maxPrefixLength: DEFAULT_MAX_PREFIX_LENGTH,
        });

        // Tear down services on each server
        for (const server of topology.servers) {
          const ssh = sshManagers.find((s) => s.getHost() === server.hostname);
          const serverLogger = serverLoggers.get(server.hostname)!;

          if (!ssh) {
            serverLogger.warn("SSH connection failed, skipping");
            continue;
          }

          try {
            // Stop DNS
            serverLogger.info("Stopping DNS service...");
            await stopCoreDNSService(ssh);

            // Stop Corrosion (if using)
            if (topology.discovery === "corrosion") {
              serverLogger.info("Stopping Corrosion service...");
              await stopCorrosionService(ssh);
            }

            // Stop WireGuard
            serverLogger.info("Stopping WireGuard interface...");
            await bringDownWireGuardInterface(ssh);
            await disableWireGuardService(ssh);

            // Remove configuration files
            serverLogger.info("Removing configuration files...");

            // Remove WireGuard config
            await ssh.executeCommand("rm -f /etc/wireguard/jiji0.conf");

            // Remove Corrosion data (this deletes all cluster state)
            await ssh.executeCommand("rm -rf /opt/jiji/corrosion");

            // Remove DNS config
            await ssh.executeCommand("rm -rf /opt/jiji/dns");

            // Remove systemd services
            await ssh.executeCommand(
              "rm -f /etc/systemd/system/jiji-corrosion.service",
            );
            await ssh.executeCommand(
              "rm -f /etc/systemd/system/jiji-dns.service",
            );
            await ssh.executeCommand(
              "rm -f /etc/systemd/system/jiji-dns-update.service",
            );
            await ssh.executeCommand(
              "rm -f /etc/systemd/system/jiji-dns-update.timer",
            );
            await ssh.executeCommand(
              "rm -f /etc/systemd/system/jiji-control-loop.service",
            );

            // Reload systemd
            await ssh.executeCommand("systemctl daemon-reload");

            serverLogger.success("Network teardown complete");
          } catch (error) {
            serverLogger.error(`Teardown failed: ${error}`);
          }
        }

        log.success(
          "Network teardown complete - all cluster state removed from Corrosion",
          "network",
        );
      });
    } catch (error) {
      await handleCommandError(error, {
        operation: "Network teardown",
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
