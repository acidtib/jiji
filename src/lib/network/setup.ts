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
import { log } from "../../utils/logger.ts";
import { SubnetAllocator } from "./subnet_allocator.ts";
import { compressIpv6, deriveManagementIp } from "./ipv6.ts";
import {
  bringUpWireGuardInterface,
  cleanupOrphanedInterfaces,
  enableWireGuardService,
  generateWireGuardKeypair,
  installWireGuard,
  restartWireGuardInterface,
  verifyWireGuardConfig,
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
  createJijiDnsService,
  installJijiDns,
  startJijiDnsService,
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
import { ensureUfwForwardRules } from "./firewall.ts";
import { createControlLoopService } from "./control_loop.ts";
import { discoverAllEndpoints, selectBestEndpoint } from "./ip_discovery.ts";
import { CORROSION_API_PORT, CORROSION_GOSSIP_PORT } from "../../constants.ts";

const WIREGUARD_PORT = 31820;
const WIREGUARD_INTERFACE = "jiji0";

/**
 * Build the network create command with appropriate options
 * Docker 28.2.0+ is required (enforced in engine.ts), so we always use trusted_host_interfaces
 */
function buildNetworkCreateCmd(
  engine: "docker" | "podman",
  networkName: string,
  containerSubnet: string,
  containerGateway: string,
): string {
  if (engine === "podman") {
    // Podman: --disable-dns prevents aardvark-dns from intercepting queries
    // Containers will use daemon-level DNS from /etc/containers/containers.conf
    return `${engine} network create ${networkName} --subnet=${containerSubnet} --gateway=${containerGateway} --disable-dns`;
  }

  // Docker 28.2.0+: use trusted_host_interfaces for explicit WireGuard routing
  // This prevents routing issues with newer Docker versions
  return `${engine} network create ${networkName} --subnet=${containerSubnet} --gateway=${containerGateway} ` +
    `--opt com.docker.network.bridge.name=jiji-br0 ` +
    `--opt com.docker.network.bridge.trusted_host_interfaces="${WIREGUARD_INTERFACE}"`;
}

/**
 * Get network subnet and gateway from JSON inspection
 * Works with both old and new podman versions, as well as docker
 */
interface NetworkInfo {
  subnet?: string;
  gateway?: string;
}

async function inspectNetworkJson(
  ssh: SSHManager,
  networkName: string,
  engine: "docker" | "podman",
): Promise<NetworkInfo> {
  const inspectCmd = `${engine} network inspect ${networkName}`;
  const result = await ssh.executeCommand(inspectCmd);

  if (result.code !== 0) {
    return {};
  }

  try {
    const networkData = JSON.parse(result.stdout);
    if (!networkData || !networkData[0]) {
      return {};
    }

    const network = networkData[0];
    const info: NetworkInfo = {};

    // Try different JSON structures for subnet
    // Podman (new): network.subnets[0].subnet
    // Podman (old): network.plugins[0].ipam.ranges[0][0].subnet
    // Docker: network.IPAM.Config[0].Subnet
    if (network.subnets && network.subnets[0]) {
      info.subnet = network.subnets[0].subnet;
      info.gateway = network.subnets[0].gateway;
    } else if (network.Subnets && network.Subnets[0]) {
      info.subnet = network.Subnets[0].Subnet;
      info.gateway = network.Subnets[0].Gateway;
    } else if (
      network.plugins && network.plugins[0] && network.plugins[0].ipam &&
      network.plugins[0].ipam.ranges && network.plugins[0].ipam.ranges[0] &&
      network.plugins[0].ipam.ranges[0][0]
    ) {
      // Old podman CNI format
      info.subnet = network.plugins[0].ipam.ranges[0][0].subnet;
      info.gateway = network.plugins[0].ipam.ranges[0][0].gateway;
    } else if (network.IPAM && network.IPAM.Config && network.IPAM.Config[0]) {
      // Docker format
      info.subnet = network.IPAM.Config[0].Subnet;
      info.gateway = network.IPAM.Config[0].Gateway;
    }

    return info;
  } catch (e) {
    log.debug(`Failed to parse network JSON: ${e}`, "network");
    return {};
  }
}

/**
 * Wait for a container network to be fully initialized and ready
 *
 * Polls the network to verify it exists and has a valid gateway IP.
 * This is more reliable than fixed delays.
 *
 * @param ssh - SSH connection to the server
 * @param networkName - Name of the network to check
 * @param engine - Container engine (docker or podman)
 * @param expectedSubnet - Expected subnet for validation
 * @param maxAttempts - Maximum number of polling attempts (default: 15)
 * @param delayMs - Delay between attempts in milliseconds (default: 1000)
 * @returns True if network is ready, throws error if timeout
 */
async function waitForNetworkReady(
  ssh: SSHManager,
  networkName: string,
  engine: "docker" | "podman",
  expectedSubnet: string,
  maxAttempts = 15,
  delayMs = 1000,
): Promise<void> {
  const host = ssh.getHost();

  for (let attempt = 1; attempt <= maxAttempts; attempt++) {
    const info = await inspectNetworkJson(ssh, networkName, engine);

    if (info.subnet === expectedSubnet && info.gateway) {
      log.debug(
        `Network ${networkName} is ready on ${host} (subnet: ${info.subnet}, gateway: ${info.gateway}, attempt ${attempt}/${maxAttempts})`,
        "network",
      );
      return;
    }

    // Network not ready yet, wait before next attempt
    if (attempt < maxAttempts) {
      log.debug(
        `Network ${networkName} not ready on ${host} (subnet: ${
          info.subnet || "none"
        }, gateway: ${
          info.gateway || "none"
        }), retrying in ${delayMs}ms (attempt ${attempt}/${maxAttempts})`,
        "network",
      );
      await new Promise((resolve) => setTimeout(resolve, delayMs));
    }
  }

  throw new Error(
    `Network ${networkName} did not become ready on ${host} after ${maxAttempts} attempts`,
  );
}

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
    // Check if networking is enabled
    if (!config.network.enabled) {
      return [];
    }

    // 1. Private Network Setup

    let topology: NetworkTopology | null = null;
    let isNewNetwork = true;
    let existingServersCount = 0;

    // Try to load topology from any existing server
    for (const ssh of sshManagers) {
      try {
        // Skip if Corrosion is not installed yet
        const { isCorrosionInstalled } = await import("./corrosion.ts");

        const installed = await isCorrosionInstalled(ssh);
        if (!installed) continue;

        topology = await loadTopology(ssh);
        if (topology !== null) {
          isNewNetwork = false;
          existingServersCount = topology.servers.length;
          break;
        }
      } catch {
        // Server doesn't have Corrosion running yet, continue
        continue;
      }
    }

    if (isNewNetwork) {
      topology = createTopology(
        config.network.clusterCidr,
        config.network.serviceDomain,
      );
    }

    log.section("Private Network Setup:");
    for (const ssh of sshManagers) {
      await log.hostBlock(ssh.getHost(), () => {
        log.say(`├── Discovery: ${config.network.discovery}`, 2);
        log.say(`├── Cluster CIDR: ${config.network.clusterCidr}`, 2);
        log.say(`├── Internal domain: ${config.network.serviceDomain}`, 2);

        if (isNewNetwork) {
          log.say(
            "└── No existing cluster found - creating new network cluster",
            2,
          );
        } else {
          log.say("├── Found existing network cluster in Corrosion", 2);
          log.say(`└── Existing servers: ${existingServersCount}`, 2);
        }
      }, { indent: 1 });
    }

    // Create subnet allocator
    const allocator = new SubnetAllocator(config.network.clusterCidr);

    // 2. Install Dependencies
    log.section("Installing Network Dependencies:");
    for (const ssh of sshManagers) {
      await log.hostBlock(ssh.getHost(), async () => {
        const host = ssh.getHost();
        try {
          // Install WireGuard
          log.say("├── Installing WireGuard", 2);
          const wgInstalled = await installWireGuard(ssh);
          if (!wgInstalled) throw new Error("Failed to install WireGuard");

          // Install Corrosion
          if (config.network.discovery === "corrosion") {
            log.say("├── Installing Corrosion", 2);
            const corrInstalled = await installCorrosion(ssh);
            if (!corrInstalled) throw new Error("Failed to install Corrosion");
          }

          // Install jiji-dns
          log.say(`└── Installing jiji-dns`, 2);
          const dnsInstalled = await installJijiDns(ssh);
          if (!dnsInstalled) throw new Error("Failed to install jiji-dns");
        } catch (error) {
          results.push({ host, success: false, error: String(error) });
          throw error;
        }
      }, { indent: 1 });
    }

    // Track existing vs new servers for WireGuard configuration
    const existingServerHosts: Set<string> = new Set();
    const newServerHosts: Set<string> = new Set();

    // 3. Generate Keys & Allocate IPs
    log.section("Generating Keys & Allocating IPs:");

    const networkServers: NetworkServer[] = [];
    // We need to loop serially to allocate IPs correctly (sequential indices)
    // although allocator handles it if we pass index.
    let newServerCount = 0;

    for (const ssh of sshManagers) {
      await log.hostBlock(ssh.getHost(), async () => {
        const host = ssh.getHost();
        try {
          // Check if server already exists
          let server = getServerByHostname(topology!, host);
          let serverIndex: number;

          if (server) {
            existingServerHosts.add(host);
            serverIndex = topology!.servers.indexOf(server);

            const endpoints = await discoverAllEndpoints(ssh, WIREGUARD_PORT);
            log.say(
              `├── Discovered ${endpoints.length} endpoint(s): ${
                endpoints.join(", ")
              }`,
              2,
            );

            log.say(`├── Server already in topology, reusing configuration`, 2);

            // Update endpoints (may have changed)
            log.debug(
              `Updating endpoints for ${host}: ${JSON.stringify(endpoints)}`,
              "network",
            );
            server.endpoints = endpoints;

            // Update keypair check...
            const { privateKey, publicKey } = await generateWireGuardKeypair(
              ssh,
            );
            server.wireguardPublicKey = publicKey;
            (server as NetworkServer & { _privateKey?: string })._privateKey =
              privateKey;
          } else {
            serverIndex = topology!.servers.length + newServerCount;
            newServerCount++;
            newServerHosts.add(host);
            const serverId = generateServerId(host, topology!);

            const endpoints = await discoverAllEndpoints(ssh, WIREGUARD_PORT);
            log.say(
              `├── Discovered ${endpoints.length} endpoint(s): ${
                endpoints.join(", ")
              }`,
              2,
            );

            const { privateKey, publicKey } = await generateWireGuardKeypair(
              ssh,
            );
            const subnet = allocator.allocateSubnet(serverIndex);
            const wireguardIp = allocator.getServerWireGuardIp(serverIndex);
            const managementIp = await deriveManagementIp(publicKey);

            server = {
              id: serverId,
              hostname: host,
              subnet,
              wireguardIp,
              wireguardPublicKey: publicKey,
              managementIp,
              endpoints,
            };
            (server as NetworkServer & { _privateKey?: string })._privateKey =
              privateKey;

            log.say(
              `├── Allocated subnet ${subnet} with WireGuard IP ${wireguardIp}`,
              2,
            );
          }

          networkServers.push(server);

          // If we are adding, we count +1.
          const currentTotal = topology!.servers.length + newServerCount; // rough estimate if we didn't add yet
          log.say(`└── Network topology has ${currentTotal} server(s)`, 2);
        } catch (error) {
          results.push({ host, success: false, error: String(error) });
          throw error;
        }
      }, { indent: 1 });
    }

    // Update topology with all servers
    for (const server of networkServers) {
      topology = addServer(topology!, server);
    }

    // 4. Configure WireGuard Mesh
    log.section("Configuring WireGuard Mesh:");
    for (const ssh of sshManagers) {
      await log.hostBlock(ssh.getHost(), async () => {
        const host = ssh.getHost();
        const server = getServerByHostname(topology!, host);
        if (!server) return;

        try {
          // Build peers
          const peers: WireGuardPeer[] = topology!.servers
            .filter((s: NetworkServer) => s.id !== server.id)
            .map((peer: NetworkServer) => {
              const peerServerIndex = parseInt(peer.subnet.split(".")[2]);
              const peerContainerThirdOctet = 128 + peerServerIndex;
              const peerBaseNetwork = peer.subnet.split(".").slice(0, 2).join(
                ".",
              );
              const peerContainerSubnet =
                `${peerBaseNetwork}.${peerContainerThirdOctet}.0/24`;

              // Select best endpoint based on network locality
              const bestEndpoint = selectBestEndpoint(
                server.endpoints,
                peer.endpoints,
              );

              return {
                publicKey: peer.wireguardPublicKey,
                allowedIps: [
                  peer.subnet,
                  peerContainerSubnet,
                  `${peer.managementIp}/128`,
                ],
                endpoint: bestEndpoint,
                persistentKeepalive: 25,
              };
            });

          const privateKey =
            (server as NetworkServer & { _privateKey?: string })._privateKey;
          if (!privateKey) throw new Error("Private key not found");

          await writeWireGuardConfig(ssh, {
            privateKey,
            address: [`${server.wireguardIp}/24`, `${server.managementIp}/128`],
            listenPort: WIREGUARD_PORT,
            mtu: 1420,
            peers,
          });

          log.say(
            `├── WireGuard config written to /etc/wireguard/jiji0.conf`,
            2,
          );

          // Clean up any orphaned interfaces that might conflict
          await cleanupOrphanedInterfaces(ssh, config.network.clusterCidr);

          const isExistingServer = existingServerHosts.has(host);
          const isNewServer = newServerHosts.has(host);

          if (isExistingServer && newServerHosts.size > 0) {
            log.say(
              `├── Restarting WireGuard to connect to ${newServerHosts.size} new server(s)`,
              2,
            );
            await restartWireGuardInterface(ssh);
          } else if (isNewServer) {
            // log.say("Setting up WireGuard interface...", 2);
            // User output: "WireGuard interface jiji0 is up on 143.110.143.43"
            await bringUpWireGuardInterface(ssh);
          } else {
            // log.say("Ensuring WireGuard interface is running...", 2);
            await bringUpWireGuardInterface(ssh);
          }

          log.say(`├── WireGuard interface jiji0 is up`, 2);

          // Verify WireGuard configuration was properly loaded
          log.debug(
            `Verifying WireGuard configuration on ${host}`,
            "network",
          );
          const verification = await verifyWireGuardConfig(ssh, peers);

          if (!verification.success) {
            const errorDetails: string[] = [
              `WireGuard configuration verification failed on ${host}:`,
            ];

            if (!verification.peerCountMatch) {
              errorDetails.push(
                `  - Expected ${peers.length} peers, found ${verification.missingPeers.length} missing`,
              );
            }

            if (verification.missingPeers.length > 0) {
              errorDetails.push(
                `  - Missing peers: ${
                  verification.missingPeers.map((p) =>
                    p.substring(0, 8) + "..."
                  ).join(", ")
                }`,
              );
            }

            if (verification.incorrectAllowedIps.length > 0) {
              errorDetails.push(`  - Incorrect AllowedIPs on peers:`);
              for (const mismatch of verification.incorrectAllowedIps) {
                errorDetails.push(`    Peer ${mismatch.peer}:`);
                errorDetails.push(
                  `      Expected: ${mismatch.expected.join(", ")}`,
                );
                errorDetails.push(
                  `      Actual:   ${mismatch.actual.join(", ")}`,
                );
              }
            }

            if (verification.details) {
              errorDetails.push(`  - Details: ${verification.details}`);
            }

            // Try reloading one more time
            log.warn(
              `First verification failed, attempting reload on ${host}`,
              "network",
            );
            await restartWireGuardInterface(ssh);

            // Verify again
            const secondVerification = await verifyWireGuardConfig(ssh, peers);
            if (!secondVerification.success) {
              errorDetails.push(
                `\nSecond reload also failed. This indicates a persistent issue.`,
              );
              throw new Error(errorDetails.join("\n"));
            }

            log.success(
              `WireGuard configuration verified after retry on ${host}`,
              "network",
            );
          } else {
            log.debug(
              `WireGuard configuration verified successfully on ${host}`,
              "network",
            );
          }

          await enableWireGuardService(ssh, "jiji0", config.builder.engine);
          log.say(
            `└── WireGuard mesh configured with ${peers.length} peer(s)`,
            2,
          );
        } catch (error) {
          results.push({ host, success: false, error: String(error) });
          throw error;
        }
      }, { indent: 1 });
    }

    // 5. Configure Corrosion
    if (config.network.discovery === "corrosion") {
      log.section("Configuring Corrosion:");
      for (const ssh of sshManagers) {
        await log.hostBlock(ssh.getHost(), async () => {
          const host = ssh.getHost();
          const server = getServerByHostname(topology!, host);
          if (!server) return;

          try {
            // Write config
            const initialPeers = topology!.servers
              .filter((s: NetworkServer) => s.id !== server.id)
              .map((peer: NetworkServer) =>
                `[${compressIpv6(peer.managementIp)}]:${CORROSION_GOSSIP_PORT}`
              );

            await writeCorrosionConfig(ssh, {
              dbPath: "/opt/jiji/corrosion/state.db",
              schemaPath: "/opt/jiji/corrosion/schemas/jiji.sql",
              gossipAddr: `[${
                compressIpv6(server.managementIp)
              }]:${CORROSION_GOSSIP_PORT}`,
              apiAddr: `127.0.0.1:${CORROSION_API_PORT}`,
              adminPath: "/var/run/jiji/corrosion-admin.sock",
              bootstrap: initialPeers,
              plaintext: true,
            });

            await createCorrosionService(ssh);
            await startCorrosionService(ssh);

            log.say("├── Waiting for Corrosion cluster to form...", 2);
            await new Promise((resolve) => setTimeout(resolve, 5000));

            if (isNewNetwork && ssh === sshManagers[0]) {
              await initializeClusterMetadata(
                ssh,
                config.network.clusterCidr,
                config.network.serviceDomain,
                config.network.discovery,
              );
            }

            log.say("├── Waiting for Corrosion database sync...", 2);
            await waitForCorrosionSync(ssh, topology!.servers.length);
            log.say("├── Corrosion database synchronized", 2);

            // Register ALL servers from topology into Corrosion on this node
            // This ensures each server has the complete list even before gossip syncs
            const now = Date.now();
            for (const topologyServer of topology!.servers) {
              await registerServer(ssh, {
                id: topologyServer.id,
                hostname: topologyServer.hostname,
                subnet: topologyServer.subnet,
                wireguardIp: topologyServer.wireguardIp,
                wireguardPublicKey: topologyServer.wireguardPublicKey,
                managementIp: topologyServer.managementIp,
                endpoints: topologyServer.endpoints,
                lastSeen: now,
              });
            }
            log.say(
              `└── Registered ${
                topology!.servers.length
              } server(s) in Corrosion`,
              2,
            );
          } catch (error) {
            results.push({ host, success: false, error: String(error) });
            throw error;
          }
        }, { indent: 1 });
      }
    }

    // 6. Configure Container Networks
    log.section("Configuring Container Networks:");
    for (const ssh of sshManagers) {
      await log.hostBlock(ssh.getHost(), async () => {
        const host = ssh.getHost();
        const server = getServerByHostname(topology!, host);
        if (!server) return;

        try {
          const networkName = "jiji";
          const engine = config.builder.engine;
          const serverIndex = parseInt(server.subnet.split(".")[2]);
          const containerThirdOctet = 128 + serverIndex;
          const baseNetwork = server.subnet.split(".").slice(0, 2).join(".");
          const containerSubnet = `${baseNetwork}.${containerThirdOctet}.0/24`;
          const containerGateway = `${baseNetwork}.${containerThirdOctet}.1`;

          // Check if network exists and has correct configuration
          const existingNetwork = await inspectNetworkJson(
            ssh,
            networkName,
            engine,
          );

          if (existingNetwork.subnet) {
            if (existingNetwork.subnet === containerSubnet) {
              log.say(
                `└── ${engine} network '${networkName}' already exists with correct configuration`,
                2,
              );
            } else {
              log.say(
                `├── ${engine} network '${networkName}' exists with incorrect subnet (${existingNetwork.subnet}, expected ${containerSubnet})`,
                2,
              );

              // Check if any containers are using the network
              const containersCmd =
                `${engine} ps -a --filter network=${networkName} --format '{{.Names}}'`;
              const containersResult = await ssh.executeCommand(containersCmd);
              const containers = containersResult.stdout.trim().split("\n")
                .filter((c) => c);

              if (containers.length > 0) {
                log.say(
                  `├── Stopping ${containers.length} container(s) using network: ${
                    containers.join(", ")
                  }`,
                  2,
                );
                for (const container of containers) {
                  await ssh.executeCommand(
                    `${engine} stop ${container} || true`,
                  );
                  await ssh.executeCommand(`${engine} rm ${container} || true`);
                }
              }

              // Remove the incorrect network
              log.say(`├── Removing network with incorrect subnet`, 2);
              const removeResult = await ssh.executeCommand(
                `${engine} network rm ${networkName} || true`,
              );
              if (removeResult.code !== 0) {
                log.warn(`Failed to remove network: ${removeResult.stderr}`);
              }

              // Recreate with correct configuration
              const createNetworkCmd = buildNetworkCreateCmd(
                engine,
                networkName,
                containerSubnet,
                containerGateway,
              );

              const networkResult = await ssh.executeCommand(createNetworkCmd);
              if (networkResult.code !== 0) {
                throw new Error(networkResult.stderr);
              }

              // Wait for network to be fully initialized and ready
              await waitForNetworkReady(
                ssh,
                networkName,
                engine,
                containerSubnet,
              );

              log.say(
                `└── ${engine} network '${networkName}' recreated: subnet=${containerSubnet}, gateway=${containerGateway}`,
                2,
              );
            }
          } else {
            // Create network
            const createNetworkCmd = buildNetworkCreateCmd(
              engine,
              networkName,
              containerSubnet,
              containerGateway,
            );

            const networkResult = await ssh.executeCommand(createNetworkCmd);
            if (networkResult.code !== 0) throw new Error(networkResult.stderr);

            // Wait for network to be fully initialized and ready
            await waitForNetworkReady(
              ssh,
              networkName,
              engine,
              containerSubnet,
            );

            log.say(
              `└── ${engine} network '${networkName}' created: subnet=${containerSubnet}, gateway=${containerGateway}`,
              2,
            );
          }
        } catch (error) {
          results.push({ host, success: false, error: String(error) });
          throw error;
        }
      }, { indent: 1 });
    }

    // 7. Configure Routing
    log.section("Configuring Routing:");
    for (const ssh of sshManagers) {
      await log.hostBlock(ssh.getHost(), async () => {
        const host = ssh.getHost();
        const server = getServerByHostname(topology!, host);
        if (!server) return;

        try {
          // Calculate container subnet for this server
          const serverIndex = parseInt(server.subnet.split(".")[2]);
          const containerThirdOctet = 128 + serverIndex;
          const baseNetwork = server.subnet.split(".").slice(0, 2).join(".");
          const containerSubnet = `${baseNetwork}.${containerThirdOctet}.0/24`;

          // Build peer list with their CONTAINER subnets (not WireGuard subnets)
          // We need to route to peer container subnets, not their WireGuard subnets
          const peers = topology!.servers
            .filter((s: NetworkServer) => s.id !== server.id)
            .map((peer: NetworkServer) => {
              // Calculate peer's container subnet the same way we calculate ours
              const peerIndex = parseInt(peer.subnet.split(".")[2]);
              const peerContainerThirdOctet = 128 + peerIndex;
              const peerBaseNetwork = peer.subnet.split(".").slice(0, 2).join(
                ".",
              );
              const peerContainerSubnet =
                `${peerBaseNetwork}.${peerContainerThirdOctet}.0/24`;

              log.debug(
                `Peer ${peer.hostname}: WireGuard subnet=${peer.subnet}, Container subnet=${peerContainerSubnet}`,
                "network",
              );

              return {
                subnet: peerContainerSubnet,
                hostname: peer.hostname,
              };
            });

          log.debug(
            `Configuring routing for ${host}: own container=${containerSubnet}, peers=${
              JSON.stringify(peers)
            }`,
            "network",
          );

          await setupServerRouting(
            ssh,
            server.subnet,
            containerSubnet,
            config.network.clusterCidr,
            peers,
            "jiji",
            config.builder.engine,
            "jiji0",
          );

          // Ensure UFW forward rules if UFW is active
          await ensureUfwForwardRules(ssh, containerSubnet);

          log.say(
            `└── Routing configured for ${peers.length} peer subnet(s)`,
            2,
          );
        } catch (error) {
          results.push({ host, success: false, error: String(error) });
          throw error;
        }
      }, { indent: 1 });
    }

    // 8. Configure DNS
    log.section("Configuring DNS:");
    for (const ssh of sshManagers) {
      await log.hostBlock(ssh.getHost(), async () => {
        const host = ssh.getHost();
        const server = getServerByHostname(topology!, host);
        if (!server) return;

        try {
          // Calculate container gateway IP for DNS to also listen on
          // This allows containers to reach the DNS server directly
          const serverIndex = parseInt(server.subnet.split(".")[2]);
          const containerThirdOctet = 128 + serverIndex;
          const baseNetwork = server.subnet.split(".").slice(0, 2).join(".");
          const containerGateway = `${baseNetwork}.${containerThirdOctet}.1`;

          // jiji-dns listens on both WireGuard IP and container gateway
          await createJijiDnsService(ssh, {
            listenAddr: `${server.wireguardIp}:53,${containerGateway}:53`,
            serviceDomain: config.network.serviceDomain,
            corrosionApiAddr: `http://127.0.0.1:${CORROSION_API_PORT}`,
          });
          await startJijiDnsService(ssh);
          log.say("├── jiji-dns service started", 2);

          await configureContainerDNS(
            ssh,
            containerGateway,
            config.network.serviceDomain,
            config.builder.engine,
          );
          log.say(
            `└── ${config.builder.engine} configured: DNS=${containerGateway}, search=${config.network.serviceDomain}`,
            2,
          );
        } catch (error) {
          results.push({ host, success: false, error: String(error) });
          throw error;
        }
      }, { indent: 1 });
    }

    // 9. Setup Network Control Loop
    log.section("Setting Up Network Control Loop:");
    for (const ssh of sshManagers) {
      await log.hostBlock(ssh.getHost(), async () => {
        const host = ssh.getHost();
        const server = getServerByHostname(topology!, host);
        if (!server) return;

        try {
          await createControlLoopService(
            ssh,
            server.id,
            config.builder.engine,
            "jiji0",
          );
          log.say("├── Network control loop configured", 2);
          log.say(
            "└── Network state stored in Corrosion distributed database",
            2,
          );
        } catch (error) {
          log.error(
            `Failed to setup network control loop on ${host}: ${error}`,
            "network",
          );
          results.push({ host, success: false, error: String(error) });
          throw error;
        }
      }, { indent: 1 });
    }

    // Cleanup private keys
    if (topology) {
      topology = {
        ...topology,
        servers: topology.servers.map((server) => {
          const { _privateKey, ...cleanServer } = server as NetworkServer & {
            _privateKey?: string;
          };
          return cleanServer;
        }),
      };
    }
    validateTopology(topology!);

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
  } catch (error) {
    log.error(`Network setup failed: ${error}`, "network");
    throw error;
  } finally {
    // Restore log level
    log.setMinLevel("info");
  }

  return results;
}
