/**
 * Network setup orchestrator
 *
 * Main orchestration logic for setting up private networking
 * during server bootstrap. Coordinates WireGuard, Corrosion, and DNS setup.
 */

import type { SSHManager } from "../../utils/ssh.ts";
import type { Configuration } from "../configuration.ts";
import type { NetworkServer, NetworkSetupResult } from "../../types/network.ts";
import { log, Logger } from "../../utils/logger.ts";
import { SubnetAllocator } from "./subnet_allocator.ts";
import { deriveManagementIp } from "./ipv6.ts";
import {
  bringUpWireGuardInterface,
  enableWireGuardService,
  generateWireGuardKeypair,
  installWireGuard,
  writeWireGuardConfig,
} from "./wireguard.ts";
import {
  createCorrosionService,
  installCorrosion,
  registerServer,
  startCorrosionService,
  writeCorrosionConfig,
} from "./corrosion.ts";
import {
  configureContainerDNS,
  createCoreDNSService,
  createHostsUpdateTimer,
  installCoreDNS,
  startCoreDNSService,
  writeCoreDNSConfig,
} from "./dns.ts";
import {
  addServer,
  createTopology,
  generateServerId,
  getServerByHostname,
  loadTopology,
  saveTopology,
  validateTopology,
} from "./topology.ts";
import type { WireGuardPeer } from "../../types/network.ts";
import { setupServerRouting } from "./routes.ts";
import { createPeerMonitorService } from "./peer_monitor.ts";

const WIREGUARD_PORT = 51820;

/**
 * Setup private networking on servers
 *
 * This is the main entry point for network setup, called from the bootstrap command.
 *
 * @param config - Jiji configuration
 * @param sshManagers - SSH connections to all servers
 * @returns Array of setup results for each server
 */
export async function setupNetwork(
  config: Configuration,
  sshManagers: SSHManager[],
): Promise<NetworkSetupResult[]> {
  const results: NetworkSetupResult[] = [];

  try {
    await log.group("Network Setup", async () => {
      // Check if networking is enabled
      if (!config.network.enabled) {
        log.info("Private networking is not enabled", "network");
        return;
      }

      log.info("Setting up private networking...", "network");
      log.info(
        `Cluster CIDR: ${config.network.clusterCidr}`,
        "network",
      );
      log.info(
        `Service domain: ${config.network.serviceDomain}`,
        "network",
      );
      log.info(`Discovery: ${config.network.discovery}`, "network");

      // Load or create network topology
      let topology = await loadTopology();
      const isNewNetwork = topology === null;

      if (isNewNetwork) {
        log.info("Creating new network topology", "network");
        topology = createTopology(
          config.network.clusterCidr,
          config.network.serviceDomain,
          config.network.discovery,
        );
      } else {
        log.info("Found existing network topology", "network");
        log.info(
          `Existing servers: ${topology!.servers.length}`,
          "network",
        );
      }

      // Create subnet allocator
      const allocator = new SubnetAllocator(config.network.clusterCidr);

      // Phase 1: Install dependencies on all servers
      await log.group("Phase 1: Install Dependencies", async () => {
        const serverLoggers = Logger.forServers(
          sshManagers.map((ssh) => ssh.getHost()),
          { maxPrefixLength: 25 },
        );

        for (const ssh of sshManagers) {
          const host = ssh.getHost();
          const serverLogger = serverLoggers.get(host)!;

          try {
            // Install WireGuard
            serverLogger.info("Installing WireGuard...");
            const wgInstalled = await installWireGuard(ssh);
            if (!wgInstalled) {
              throw new Error("Failed to install WireGuard");
            }

            // Install Corrosion (if using corrosion discovery)
            if (config.network.discovery === "corrosion") {
              serverLogger.info("Installing Corrosion...");
              const corrInstalled = await installCorrosion(ssh);
              if (!corrInstalled) {
                throw new Error("Failed to install Corrosion");
              }
            }

            // Install CoreDNS
            serverLogger.info("Installing CoreDNS...");
            const dnsInstalled = await installCoreDNS(ssh);
            if (!dnsInstalled) {
              throw new Error("Failed to install CoreDNS");
            }

            serverLogger.success("Dependencies installed");
          } catch (error) {
            serverLogger.error(`Installation failed: ${error}`);
            results.push({
              host,
              success: false,
              error: String(error),
            });
          }
        }
      });

      // Phase 2: Generate WireGuard keys and allocate IPs
      await log.group("Phase 2: Generate Keys & Allocate IPs", async () => {
        const networkServers: NetworkServer[] = [];
        let newServerCount = 0; // Track only new servers being added

        for (const ssh of sshManagers) {
          const host = ssh.getHost();

          try {
            // Check if server already exists in topology
            let server = getServerByHostname(topology!, host);
            let serverIndex: number;

            if (server) {
              log.info(
                `Server ${host} already in topology, reusing`,
                "network",
              );
              serverIndex = topology!.servers.indexOf(server);

              // Generate new WireGuard keypair (private key is not stored in topology)
              const { privateKey, publicKey } = await generateWireGuardKeypair(
                ssh,
              );

              // Update server's public key if it changed
              server.wireguardPublicKey = publicKey;

              // Store private key temporarily for config writing
              (server as NetworkServer & { _privateKey?: string })._privateKey =
                privateKey;
            } else {
              // Generate new server entry
              // Calculate index based on existing servers + new servers processed so far
              serverIndex = topology!.servers.length + newServerCount;
              newServerCount++;
              const serverId = generateServerId(host, topology!);

              log.info(
                `Allocating resources for ${host} (index ${serverIndex})`,
                "network",
              );

              // Generate WireGuard keypair
              const { privateKey, publicKey } = await generateWireGuardKeypair(
                ssh,
              );

              // Allocate subnet and IPs
              const subnet = allocator.allocateSubnet(serverIndex);
              const wireguardIp = allocator.getServerWireGuardIp(serverIndex);

              // Derive management IP from public key
              const managementIp = await deriveManagementIp(publicKey);

              // Create server entry
              server = {
                id: serverId,
                hostname: host,
                subnet,
                wireguardIp,
                wireguardPublicKey: publicKey,
                managementIp,
                endpoints: [`${host}:${WIREGUARD_PORT}`],
              };

              // Store private key in topology temporarily (will be removed after config write)
              (server as NetworkServer & { _privateKey?: string })._privateKey =
                privateKey;

              log.success(
                `Allocated ${subnet} with WireGuard IP ${wireguardIp} to ${host}`,
                "network",
              );
            }

            networkServers.push(server);
          } catch (error) {
            log.error(`Failed to setup ${host}: ${error}`, "network");
            results.push({
              host,
              success: false,
              error: String(error),
            });
          }
        }

        // Update topology with all servers
        for (const server of networkServers) {
          topology = addServer(topology!, server);
        }

        log.info(
          `Network topology has ${topology!.servers.length} servers`,
          "network",
        );
      });

      // Phase 3: Configure WireGuard mesh
      await log.group("Phase 3: Configure WireGuard Mesh", async () => {
        const serverLoggers = Logger.forServers(
          sshManagers.map((ssh) => ssh.getHost()),
          { maxPrefixLength: 25 },
        );

        for (const ssh of sshManagers) {
          const host = ssh.getHost();
          const serverLogger = serverLoggers.get(host)!;
          const server = getServerByHostname(topology!, host);

          if (!server) {
            serverLogger.error("Server not found in topology");
            continue;
          }

          try {
            // Build peer list (all other servers)
            const peers: WireGuardPeer[] = topology!.servers
              .filter((s) => s.id !== server.id)
              .map((peer) => ({
                publicKey: peer.wireguardPublicKey,
                allowedIps: [peer.subnet, `${peer.managementIp}/128`],
                endpoint: peer.endpoints[0],
                persistentKeepalive: 25,
              }));

            // Write WireGuard config
            const privateKey =
              (server as NetworkServer & { _privateKey?: string })._privateKey;
            if (!privateKey) {
              throw new Error("Private key not found");
            }

            await writeWireGuardConfig(ssh, {
              privateKey,
              address: [
                `${server.wireguardIp}/24`,
                `${server.managementIp}/128`,
              ],
              listenPort: WIREGUARD_PORT,
              peers,
            });

            // Bring up interface
            await bringUpWireGuardInterface(ssh);

            // Enable service
            await enableWireGuardService(ssh);

            serverLogger.success(
              `WireGuard mesh configured with ${peers.length} peers`,
            );
          } catch (error) {
            serverLogger.error(`WireGuard setup failed: ${error}`);
            results.push({
              host,
              success: false,
              error: String(error),
            });
          }
        }
      });

      // Phase 4: Configure Corrosion (if enabled)
      if (config.network.discovery === "corrosion") {
        await log.group("Phase 4: Configure Corrosion", async () => {
          const serverLoggers = Logger.forServers(
            sshManagers.map((ssh) => ssh.getHost()),
            { maxPrefixLength: 25 },
          );

          for (const ssh of sshManagers) {
            const host = ssh.getHost();
            const serverLogger = serverLoggers.get(host)!;
            const server = getServerByHostname(topology!, host);

            if (!server) {
              serverLogger.error("Server not found in topology");
              continue;
            }

            try {
              // Build bootstrap peer list (all other servers)
              const bootstrapPeers = topology!.servers
                .filter((s) => s.id !== server.id)
                .map((peer) => `[${peer.managementIp}]:8787`);

              // Write Corrosion config
              await writeCorrosionConfig(ssh, {
                dbPath: "/opt/jiji/corrosion/state.db",
                schemaPath: "/opt/jiji/corrosion/schemas",
                gossipAddr: `[${server.managementIp}]:8787`,
                apiAddr: "127.0.0.1:8080",
                adminPath: "/var/run/jiji/corrosion-admin.sock",
                bootstrap: bootstrapPeers,
                plaintext: true,
              });

              // Create and start service
              await createCorrosionService(ssh);
              await startCorrosionService(ssh);

              // Register this server in Corrosion
              await registerServer(ssh, {
                id: server.id,
                hostname: server.hostname,
                subnet: server.subnet,
                wireguardIp: server.wireguardIp,
                wireguardPublicKey: server.wireguardPublicKey,
                managementIp: server.managementIp,
                endpoints: JSON.stringify(server.endpoints),
                lastSeen: Date.now(),
              });

              serverLogger.success(
                `Corrosion configured with ${bootstrapPeers.length} peers`,
              );
            } catch (error) {
              serverLogger.error(`Corrosion setup failed: ${error}`);
              results.push({
                host,
                success: false,
                error: String(error),
              });
            }
          }
        });
      }

      // Phase 5: Configure Container Networks
      await log.group("Phase 5: Configure Container Networks", async () => {
        const serverLoggers = Logger.forServers(
          sshManagers.map((ssh) => ssh.getHost()),
          { maxPrefixLength: 25 },
        );

        for (const ssh of sshManagers) {
          const host = ssh.getHost();
          const serverLogger = serverLoggers.get(host)!;
          const server = getServerByHostname(topology!, host);

          if (!server) {
            serverLogger.error("Server not found in topology");
            continue;
          }

          try {
            // Create container network with allocated subnet
            const networkName = "jiji";
            const engine = config.builder.engine;

            // Check if network already exists
            const checkCmd =
              `${engine} network inspect ${networkName} >/dev/null 2>&1`;
            const checkResult = await ssh.executeCommand(checkCmd);

            if (checkResult.code === 0) {
              // Network already exists - remove and recreate to ensure correct config
              serverLogger.info(
                `Removing existing ${engine} network '${networkName}'`,
              );
              await ssh.executeCommand(`${engine} network rm ${networkName}`);
            }

            // Create network with allocated subnet and gateway
            // This ensures containers get IPs from the WireGuard subnet
            const createNetworkCmd = engine === "podman"
              ? `${engine} network create ${networkName} --subnet=${server.subnet} --gateway=${server.wireguardIp} --dns=${server.wireguardIp} --dns=8.8.8.8`
              : `${engine} network create ${networkName} --subnet=${server.subnet} --gateway=${server.wireguardIp} --opt com.docker.network.bridge.name=jiji-br0`;

            const networkResult = await ssh.executeCommand(createNetworkCmd);

            if (networkResult.code !== 0) {
              throw new Error(
                `Failed to create ${engine} network: ${networkResult.stderr}`,
              );
            }

            serverLogger.success(
              `${engine} network '${networkName}' created: subnet=${server.subnet}, gateway=${server.wireguardIp}`,
            );
          } catch (error) {
            serverLogger.error(`Container network setup failed: ${error}`);
            results.push({
              host,
              success: false,
              error: String(error),
            });
          }
        }
      });

      // Phase 6: Configure Routing
      await log.group("Phase 6: Configure Routing", async () => {
        const serverLoggers = Logger.forServers(
          sshManagers.map((ssh) => ssh.getHost()),
          { maxPrefixLength: 25 },
        );

        for (const ssh of sshManagers) {
          const host = ssh.getHost();
          const serverLogger = serverLoggers.get(host)!;
          const server = getServerByHostname(topology!, host);

          if (!server) {
            serverLogger.error("Server not found in topology");
            continue;
          }

          try {
            // Build peer list for routing
            const peers = topology!.servers
              .filter((s) => s.id !== server.id)
              .map((peer) => ({
                subnet: peer.subnet,
                hostname: peer.hostname,
              }));

            // Setup all routing configuration
            await setupServerRouting(
              ssh,
              server.subnet,
              peers,
              "jiji",
              config.builder.engine,
              "jiji0",
            );

            serverLogger.success(
              `Routing configured for ${peers.length} peer subnets`,
            );
          } catch (error) {
            serverLogger.error(`Routing setup failed: ${error}`);
            results.push({
              host,
              success: false,
              error: String(error),
            });
          }
        }
      });

      // Phase 7: Configure DNS
      await log.group("Phase 7: Configure DNS", async () => {
        const serverLoggers = Logger.forServers(
          sshManagers.map((ssh) => ssh.getHost()),
          { maxPrefixLength: 25 },
        );

        for (const ssh of sshManagers) {
          const host = ssh.getHost();
          const serverLogger = serverLoggers.get(host)!;
          const server = getServerByHostname(topology!, host);

          if (!server) {
            serverLogger.error("Server not found in topology");
            continue;
          }

          try {
            // Write CoreDNS config
            await writeCoreDNSConfig(ssh, {
              listenAddr: `${server.wireguardIp}:53`,
              serviceDomain: config.network.serviceDomain,
              corrosionApiAddr: "127.0.0.1:8080",
              upstreamResolvers: ["8.8.8.8", "1.1.1.1"],
            });

            // Create DNS service
            await createCoreDNSService(ssh, `${server.wireguardIp}:53`);

            // Create periodic update timer
            await createHostsUpdateTimer(ssh, 30);

            // Start DNS
            await startCoreDNSService(ssh);

            // Configure container engine to use this DNS
            await configureContainerDNS(
              ssh,
              server.wireguardIp,
              config.builder.engine,
            );

            serverLogger.success("DNS configured");
          } catch (error) {
            serverLogger.error(`DNS setup failed: ${error}`);
            results.push({
              host,
              success: false,
              error: String(error),
            });
          }
        }
      });

      // Phase 8: Setup Peer Monitoring
      await log.group("Phase 8: Setup Peer Monitoring", async () => {
        const serverLoggers = Logger.forServers(
          sshManagers.map((ssh) => ssh.getHost()),
          { maxPrefixLength: 25 },
        );

        for (const ssh of sshManagers) {
          const host = ssh.getHost();
          const serverLogger = serverLoggers.get(host)!;
          const server = getServerByHostname(topology!, host);

          if (!server) {
            serverLogger.error("Server not found in topology");
            continue;
          }

          try {
            // Create peer monitoring service
            await createPeerMonitorService(
              ssh,
              topology!.servers,
              server.id,
              "jiji0",
              60, // Check every 60 seconds
            );

            serverLogger.success("Peer monitoring configured");
          } catch (error) {
            serverLogger.error(`Peer monitoring setup failed: ${error}`);
            // Don't fail the entire setup for monitoring issues
            serverLogger.warn("Continuing without peer monitoring");
          }
        }
      });

      // Clean up temporary private keys from topology
      for (const server of topology!.servers) {
        delete (server as NetworkServer & { _privateKey?: string })._privateKey;
      }

      // Validate and save topology
      validateTopology(topology!);
      await saveTopology(topology!);

      log.success(
        `Private network setup complete with ${
          topology!.servers.length
        } servers`,
        "network",
      );
      log.info(`Network state saved to .jiji/network.json`, "network");

      // Mark all as successful if no errors so far
      if (results.length === 0) {
        for (const ssh of sshManagers) {
          results.push({
            host: ssh.getHost(),
            success: true,
            message: "Network setup successful",
          });
        }
      }
    });
  } catch (error) {
    log.error(`Network setup failed: ${error}`, "network");
    throw error;
  }

  return results;
}
