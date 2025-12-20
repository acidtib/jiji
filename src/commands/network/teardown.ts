/**
 * Network teardown command
 *
 * Tears down the private network by stopping services,
 * removing configurations, and cleaning up state.
 */

import { Command } from "@cliffy/command";
import { Configuration } from "../../lib/configuration.ts";
import { loadTopology } from "../../lib/network/topology.ts";
import { setupSSHConnections } from "../../utils/ssh.ts";
import {
  bringDownWireGuardInterface,
  disableWireGuardService,
} from "../../lib/network/wireguard.ts";
import { stopCorrosionService } from "../../lib/network/corrosion.ts";
import { stopCoreDNSService } from "../../lib/network/dns.ts";
import { log, Logger } from "../../utils/logger.ts";
import type { GlobalOptions } from "../../types.ts";

export const teardownCommand = new Command()
  .description("Tear down private network")
  .action(async (options) => {
    try {
      await log.group("Network Teardown", async () => {
        // Cast options to GlobalOptions
        const globalOptions = options as unknown as GlobalOptions;

        // Load configuration for SSH settings
        const config = await Configuration.load(
          globalOptions.environment,
          globalOptions.configFile,
        );

        // Get all server hostnames from deploy.yml
        const hostnames = config.getAllServerHosts();

        if (hostnames.length === 0) {
          log.info("No servers found in configuration", "network");
          log.info("Nothing to tear down", "network");
          return;
        }

        // Connect to servers
        const { managers: sshManagers } = await setupSSHConnections(
          hostnames,
          {
            user: config.ssh.user,
            port: config.ssh.port,
            proxy: config.ssh.proxy,
            proxy_command: config.ssh.proxyCommand,
            keys: config.ssh.allKeys.length > 0
              ? config.ssh.allKeys
              : undefined,
            keyData: config.ssh.keyData,
            keysOnly: config.ssh.keysOnly,
            dnsRetries: config.ssh.dnsRetries,
          },
          { allowPartialConnection: true },
        );

        if (sshManagers.length === 0) {
          log.error("Could not connect to any servers", "network");
          return;
        }

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

        try {
          const serverLoggers = Logger.forServers(hostnames, {
            maxPrefixLength: 25,
          });

          // Tear down services on each server
          for (const server of topology.servers) {
            const ssh = sshManagers.find((s) =>
              s.getHost() === server.hostname
            );
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
        } finally {
          // Clean up SSH connections
          for (const ssh of sshManagers) {
            try {
              ssh.dispose();
            } catch (error) {
              log.debug(`Failed to dispose SSH connection: ${error}`, "ssh");
            }
          }
        }
      });
    } catch (error) {
      const errorMessage = error instanceof Error
        ? error.message
        : String(error);
      log.error("Failed to tear down network:", "network");
      log.error(errorMessage, "network");
      Deno.exit(1);
    }
  });
