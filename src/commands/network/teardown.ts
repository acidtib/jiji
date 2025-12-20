/**
 * Network teardown command
 *
 * Tears down the private network by stopping services,
 * removing configurations, and cleaning up state.
 */

import { Command } from "@cliffy/command";
import { Configuration } from "../../lib/configuration.ts";
import { deleteTopology, loadTopology } from "../../lib/network/topology.ts";
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
  .option(
    "--keep-config",
    "Keep network.json file (don't delete topology)",
  )
  .action(async (options) => {
    try {
      await log.group("Network Teardown", async () => {
        // Load topology
        const topology = await loadTopology();

        if (!topology) {
          log.info("No network topology found", "network");
          log.info("Nothing to tear down", "network");
          return;
        }

        log.warn(
          "This will tear down the private network on all servers",
          "network",
        );
        log.info(`Servers: ${topology.servers.length}`, "network");

        // Cast options to get flags
        const keepConfig = (options as { keepConfig?: boolean }).keepConfig;

        // Cast options to GlobalOptions
        const globalOptions = options as unknown as GlobalOptions;

        // Load configuration for SSH settings
        const config = await Configuration.load(
          globalOptions.environment,
          globalOptions.configFile,
        );

        // Connect to servers
        const hostnames = topology.servers.map((s) => s.hostname);
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

              // Optionally remove configuration files
              if (!keepConfig) {
                serverLogger.info("Removing configuration files...");

                // Remove WireGuard config
                await ssh.executeCommand("rm -f /etc/wireguard/jiji0.conf");

                // Remove Corrosion data
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

                // Reload systemd
                await ssh.executeCommand("systemctl daemon-reload");
              }

              serverLogger.success("Network teardown complete");
            } catch (error) {
              serverLogger.error(`Teardown failed: ${error}`);
            }
          }

          // Delete topology file (unless --keep-config)
          if (!keepConfig) {
            await deleteTopology();
            log.success("Network topology deleted", "network");
          } else {
            log.info(
              "Network topology preserved (use --keep-config=false to remove)",
              "network",
            );
          }

          log.success("Network teardown complete", "network");
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
