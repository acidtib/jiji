/**
 * Network setup orchestrator
 *
 * Main orchestration logic for setting up private networking
 * during server initialization. Coordinates WireGuard, Corrosion, and DNS setup.
 */

import type { SSHManager } from "../../utils/ssh.ts";
import type { Configuration } from "../configuration.ts";
import type {
  NetworkServer,
  NetworkSetupResult,
  NetworkTopology,
  WireGuardPeer,
} from "../../types/network.ts";
import { log, Logger } from "../../utils/logger.ts";
import { SubnetAllocator } from "./subnet_allocator.ts";
import { compressIpv6, deriveManagementIp } from "./ipv6.ts";
import {
  bringUpWireGuardInterface,
  enableWireGuardService,
  generateWireGuardKeypair,
  installWireGuard,
  restartWireGuardInterface,
  writeWireGuardConfig,
} from "./wireguard.ts";
import {
  createCorrosionService,
  initializeClusterMetadata,
  installCorrosion,
  registerServer,
  startCorrosionService,
  waitForCorrosionSync,
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
  validateTopology,
} from "./topology.ts";
import { setupServerRouting } from "./routes.ts";
import { createControlLoopService } from "./control_loop.ts";
import { discoverAllEndpoints } from "./ip_discovery.ts";

const WIREGUARD_PORT = 51820;

/**
 * Setup private networking on servers
 *
 * This is the main entry point for network setup, called from the init command.
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

      // Check if cluster already exists by trying to load from Corrosion via any SSH connection
      let topology: NetworkTopology | null = null;
      let isNewNetwork = true;

      // Try to load topology from any existing server
      for (const ssh of sshManagers) {
        try {
          // Skip if Corrosion is not installed yet
          const { isCorrosionInstalled } = await import("./corrosion.ts");
          const installed = await isCorrosionInstalled(ssh);
          if (!installed) {
            continue;
          }

          topology = await loadTopology(ssh);
          if (topology !== null) {
            isNewNetwork = false;
            log.info("Found existing network cluster in Corrosion", "network");
            log.info(
              `Existing servers: ${topology.servers.length}`,
              "network",
            );
            break;
          }
        } catch {
          // Server doesn't have Corrosion running yet, continue
          continue;
        }
      }

      if (isNewNetwork) {
        log.info("Creating new network cluster", "network");
        topology = createTopology(
          config.network.clusterCidr,
          config.network.serviceDomain,
          config.network.discovery,
        );
      }

      // Create subnet allocator
      const allocator = new SubnetAllocator(config.network.clusterCidr);

      // Install dependencies on all servers
      await log.group("Install Dependencies", async () => {
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

      // Track existing vs new servers for WireGuard configuration
      const existingServerHosts: Set<string> = new Set();
      const newServerHosts: Set<string> = new Set();

      // Generate WireGuard keys and allocate IPs
      await log.group("Generate Keys & Allocate IPs", async () => {
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
              existingServerHosts.add(host);
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
              newServerHosts.add(host);
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

              // Discover all endpoints (public and private IPs)
              log.info(`Discovering endpoints for ${host}...`, "network");
              const endpoints = await discoverAllEndpoints(ssh, WIREGUARD_PORT);

              // Create server entry
              server = {
                id: serverId,
                hostname: host,
                subnet,
                wireguardIp,
                wireguardPublicKey: publicKey,
                managementIp,
                endpoints,
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

      // Configure WireGuard mesh
      await log.group("Configure WireGuard Mesh", async () => {
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
              .filter((s: NetworkServer) => s.id !== server.id)
              .map((peer: NetworkServer) => {
                // Calculate peer's container subnet using same formula as container network setup
                const peerServerIndex = parseInt(peer.subnet.split(".")[2]); // Extract third octet
                const peerContainerThirdOctet = 128 + peerServerIndex; // Offset into upper half of /16
                const peerBaseNetwork = peer.subnet.split(".").slice(0, 2).join(
                  ".",
                ); // e.g., "10.210"
                const peerContainerSubnet =
                  `${peerBaseNetwork}.${peerContainerThirdOctet}.0/24`;

                return {
                  publicKey: peer.wireguardPublicKey,
                  allowedIps: [
                    peer.subnet,
                    peerContainerSubnet,
                    `${peer.managementIp}/128`,
                  ],
                  endpoint: peer.endpoints[0],
                  persistentKeepalive: 25,
                };
              });

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
              mtu: 1420, // Standard WireGuard MTU to avoid fragmentation
              peers,
            });

            // Determine if this is an existing server or new server
            const isExistingServer = existingServerHosts.has(host);
            const isNewServer = newServerHosts.has(host);

            if (isExistingServer && newServerHosts.size > 0) {
              // Existing server with new peers added - restart interface
              serverLogger.info(
                `Restarting WireGuard interface to connect to ${newServerHosts.size} new server(s)...`,
              );
              await restartWireGuardInterface(ssh);
            } else if (isNewServer) {
              // New server - bring up interface fresh
              serverLogger.info(
                "Setting up WireGuard interface for new server...",
              );
              await bringUpWireGuardInterface(ssh);
            } else {
              // Existing server with no new peers - just ensure interface is up
              serverLogger.info("Ensuring WireGuard interface is running...");
              await bringUpWireGuardInterface(ssh);
            }

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

      // Configure Corrosion (if enabled)
      if (config.network.discovery === "corrosion") {
        await log.group("Configure Corrosion", async () => {
          const serverLoggers = Logger.forServers(
            sshManagers.map((ssh) => ssh.getHost()),
            { maxPrefixLength: 25 },
          );

          // Step 1: Write config and start Corrosion on all servers
          for (const ssh of sshManagers) {
            const host = ssh.getHost();
            const serverLogger = serverLoggers.get(host)!;
            const server = getServerByHostname(topology!, host);

            if (!server) {
              serverLogger.error("Server not found in topology");
              continue;
            }

            try {
              // Build initial peer list (all other servers)
              // Compress IPv6 addresses to match how the kernel assigns them
              const initialPeers = topology!.servers
                .filter((s: NetworkServer) => s.id !== server.id)
                .map((peer: NetworkServer) =>
                  `[${compressIpv6(peer.managementIp)}]:8787`
                );

              // Write Corrosion config
              await writeCorrosionConfig(ssh, {
                dbPath: "/opt/jiji/corrosion/state.db",
                schemaPath: "/opt/jiji/corrosion/schemas",
                gossipAddr: `[${compressIpv6(server.managementIp)}]:8787`,
                apiAddr: "127.0.0.1:8080",
                adminPath: "/var/run/jiji/corrosion-admin.sock",
                bootstrap: initialPeers,
                plaintext: true,
              });

              // Create and start service
              await createCorrosionService(ssh);
              await startCorrosionService(ssh);

              serverLogger.success("Corrosion service started");
            } catch (error) {
              serverLogger.error(`Corrosion startup failed: ${error}`);
              results.push({
                host,
                success: false,
                error: String(error),
              });
            }
          }

          // Step 2: Wait for cluster to form (all servers must be running first)
          log.info("Waiting for Corrosion cluster to form...");
          await new Promise((resolve) => setTimeout(resolve, 5000)); // Give gossip protocol time to connect

          // Step 3: Initialize cluster metadata (only on new clusters)
          if (isNewNetwork) {
            const firstSsh = sshManagers[0];
            try {
              await initializeClusterMetadata(
                firstSsh,
                config.network.clusterCidr,
                config.network.serviceDomain,
                config.network.discovery,
              );
              log.success(
                "Cluster metadata initialized in Corrosion",
                "network",
              );
            } catch (error) {
              log.error(
                `Failed to initialize cluster metadata: ${error}`,
                "network",
              );
              throw error;
            }
          }

          // Step 4: Wait for sync and register servers
          for (const ssh of sshManagers) {
            const host = ssh.getHost();
            const serverLogger = serverLoggers.get(host)!;
            const server = getServerByHostname(topology!, host);

            if (!server) {
              continue;
            }

            try {
              // Wait for Corrosion to sync with cluster
              // This prevents the "empty machines list" bug where a new server
              // would read stale state before replication completes
              serverLogger.info("Waiting for Corrosion database sync...");
              await waitForCorrosionSync(ssh, topology!.servers.length);
              serverLogger.success("Corrosion database synchronized");

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

              serverLogger.success("Server registered in Corrosion");
            } catch (error) {
              serverLogger.error(
                `Corrosion sync/registration failed: ${error}`,
              );
              results.push({
                host,
                success: false,
                error: String(error),
              });
            }
          }
        });
      }

      // Configure Container Networks
      await log.group("Configure Container Networks", async () => {
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

            // Calculate expected network configuration
            // WireGuard uses 10.210.0-127.0/24 range, containers use 10.210.128-255.0/24
            // This ensures no overlap with WireGuard interface subnets
            const serverIndex = parseInt(server.subnet.split(".")[2]); // Extract third octet
            const containerThirdOctet = 128 + serverIndex; // Offset into upper half of /16
            const baseNetwork = server.subnet.split(".").slice(0, 2).join("."); // e.g., "10.210"
            const containerSubnet =
              `${baseNetwork}.${containerThirdOctet}.0/24`;
            const containerGateway = `${baseNetwork}.${containerThirdOctet}.1`;

            // Check if network already exists
            const inspectCmd =
              `${engine} network inspect ${networkName} --format '{{range .Subnets}}{{.Subnet}}{{end}}'`;
            const inspectResult = await ssh.executeCommand(inspectCmd);

            if (inspectResult.code === 0) {
              // Network exists - verify it has correct configuration
              const existingSubnet = inspectResult.stdout.trim();

              if (existingSubnet === containerSubnet) {
                serverLogger.info(
                  `${engine} network '${networkName}' already exists with correct configuration`,
                );
                return; // Network is correctly configured, skip creation
              } else {
                // Network exists but has wrong subnet - this is a warning, don't disrupt running containers
                serverLogger.warn(
                  `${engine} network '${networkName}' exists with incorrect subnet (${existingSubnet}, expected ${containerSubnet}). ` +
                    `Skipping network recreation to avoid disrupting running containers. ` +
                    `To fix, manually remove the network when no containers are using it: ${engine} network rm ${networkName}`,
                );
                return; // Skip to avoid disrupting services
              }
            }

            // Network doesn't exist - create it
            // Note: DNS is configured at daemon level, not per-network, so all containers get service discovery
            const createNetworkCmd = engine === "podman"
              ? `${engine} network create ${networkName} --subnet=${containerSubnet} --gateway=${containerGateway}`
              : `${engine} network create ${networkName} --subnet=${containerSubnet} --gateway=${containerGateway} --opt com.docker.network.bridge.name=jiji-br0`;

            const networkResult = await ssh.executeCommand(createNetworkCmd);

            if (networkResult.code !== 0) {
              throw new Error(
                `Failed to create ${engine} network: ${networkResult.stderr}`,
              );
            }

            serverLogger.success(
              `${engine} network '${networkName}' created: subnet=${containerSubnet}, gateway=${containerGateway}`,
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

      // Configure Routing
      await log.group("Configure Routing", async () => {
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
              .filter((s: NetworkServer) => s.id !== server.id)
              .map((peer: NetworkServer) => ({
                subnet: peer.subnet,
                hostname: peer.hostname,
              }));

            // Setup all routing configuration
            await setupServerRouting(
              ssh,
              server.subnet,
              config.network.clusterCidr,
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

      // Configure DNS
      await log.group("Configure DNS", async () => {
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
              config.network.serviceDomain,
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

      // Setup Network Control Loop
      await log.group("Setup Network Control Loop", async () => {
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
            // Create unified control loop service
            // This replaces the old peer monitor and adds:
            // - Dynamic peer reconfiguration
            // - Container health tracking
            // - Endpoint rotation
            // - Heartbeat updates
            await createControlLoopService(
              ssh,
              server.id,
              config.builder.engine,
              "jiji0",
            );

            serverLogger.success("Network control loop configured");
          } catch (error) {
            serverLogger.error(`Control loop setup failed: ${error}`);
            // Don't fail the entire setup for monitoring issues
            serverLogger.warn("Continuing without control loop");
          }
        }
      });

      // Clean up temporary private keys from topology
      for (const server of topology!.servers) {
        delete (server as NetworkServer & { _privateKey?: string })._privateKey;
      }

      // Validate topology
      validateTopology(topology!);

      log.success(
        `Private network setup complete with ${
          topology!.servers.length
        } servers`,
        "network",
      );
      log.info(
        `Network state stored in Corrosion distributed database`,
        "network",
      );

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
