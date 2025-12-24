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
import { log } from "../../utils/logger.ts";
import type { GlobalOptions } from "../../types.ts";

export const teardownCommand = new Command()
  .description("Tear down private network")
  .action(async (options) => {
    const globalOptions = options as unknown as GlobalOptions;
    let ctx: Awaited<ReturnType<typeof setupCommandContext>> | undefined;

    try {
      log.section("Network Teardown:");

      const { Configuration } = await import("../../lib/configuration.ts");
      const config = await Configuration.load(
        globalOptions.environment,
        globalOptions.configFile,
      );

      const configPath = config.configPath || "unknown";
      const allHosts = config.getAllServerHosts();

      log.say(`Configuration loaded from: ${configPath}`, 1);
      log.say(`Container engine: ${config.builder.engine}`, 1);
      log.say(
        `Found ${allHosts.length} remote host(s): ${allHosts.join(", ")}`,
        1,
      );

      ctx = await setupCommandContext(globalOptions);
      const { sshManagers, targetHosts } = ctx;

      // Show connection status for each host
      console.log(""); // Empty line
      for (const ssh of sshManagers) {
        log.remote(ssh.getHost(), ": Connected", { indent: 1 });
      }

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
        log.say("- No network cluster found", 1);
        log.say("- Nothing to tear down", 1);
        return;
      }

      log.section("Tearing Down Network:");
      log.say(
        `- This will tear down the private network on all servers`,
        1,
      );
      log.say(`- Servers: ${topology.servers.length}`, 1);

      // Tear down services on each server
      for (const server of topology.servers) {
        const ssh = sshManagers.find((s) => s.getHost() === server.hostname);

        if (!ssh) {
          log.say(`- ${server.hostname}: SSH connection failed, skipping`, 1);
          continue;
        }

        await log.hostBlock(server.hostname, async () => {
          try {
            log.say("├── Stopping DNS service", 2);
            await stopCoreDNSService(ssh);

            if (topology.discovery === "corrosion") {
              log.say("├── Stopping Corrosion service", 2);
              await stopCorrosionService(ssh);
            }

            log.say("├── Stopping WireGuard interface", 2);
            await bringDownWireGuardInterface(ssh);
            await disableWireGuardService(ssh);

            log.say("├── Removing configuration files", 2);

            await ssh.executeCommand("rm -f /etc/wireguard/jiji0.conf");
            await ssh.executeCommand("rm -rf /opt/jiji/corrosion");
            await ssh.executeCommand("rm -rf /opt/jiji/dns");

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

            await ssh.executeCommand("systemctl daemon-reload");

            log.say("└── Network teardown complete", 2);
          } catch (error) {
            log.say(`└── Teardown failed: ${error}`, 2);
          }
        }, { indent: 1 });
      }

      log.success(
        "\nNetwork teardown complete - all cluster state removed",
        0,
      );
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
