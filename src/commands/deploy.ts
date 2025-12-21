import { Command } from "@cliffy/command";
import { Configuration } from "../lib/configuration.ts";
import { setupSSHConnections, type SSHManager } from "../utils/ssh.ts";
import { createServerAuditLogger } from "../utils/audit.ts";
import { log } from "../utils/logger.ts";
import { extractAppPort, ProxyCommands } from "../utils/proxy.ts";
import {
  buildAllMountArgs,
  prepareMountDirectories,
  prepareMountFiles,
} from "../utils/mount_manager.ts";
import { PortForwardManager } from "../utils/port_forward.ts";
import { VersionManager } from "../utils/version_manager.ts";
import { RegistryManager } from "../utils/registry_manager.ts";
import { filterServicesByPatterns } from "../utils/config.ts";
import { BuildService } from "../lib/services/build_service.ts";
import { ContainerRunBuilder } from "../lib/services/container_run_builder.ts";
import {
  cleanupServiceContainers,
  registerContainerClusterWide,
  registerContainerInNetwork,
} from "../lib/services/container_registry.ts";
import { getServerByHostname, loadTopology } from "../lib/network/topology.ts";

import type { GlobalOptions } from "../types.ts";

interface DeployOptions extends GlobalOptions {
  build?: boolean;
  noCache?: boolean;
}

export const deployCommand = new Command()
  .description("Deploy services to servers")
  .option("--build", "Build images before deploying", { default: false })
  .option("--no-cache", "Build without using cache (requires --build)")
  .action(async (options) => {
    let uniqueHosts: string[] = [];
    let config: Configuration | undefined;
    let sshManagers: SSHManager[] | undefined;
    let portForwardManager: PortForwardManager | undefined;

    try {
      await log.group("Service Deployment", async () => {
        log.info("Starting service deployment process", "deploy");

        // Cast options to DeployOptions
        const deployOptions = options as unknown as DeployOptions;
        const globalOptions = options as unknown as GlobalOptions;

        // Load configuration
        config = await Configuration.load(
          globalOptions.environment,
          globalOptions.configFile,
        );
        const configPath = config.configPath || "unknown";
        log.success(`Configuration loaded from: ${configPath}`, "config");
        log.info(`Container engine: ${config.builder.engine}`, "engine");

        if (!config) throw new Error("Configuration failed to load");

        // Build images if --build flag is set
        if (deployOptions.build) {
          await log.group("Service Build", async () => {
            log.info("Building service images", "build");

            // Get services to build
            const servicesToBuild = config!.getBuildServices();

            if (servicesToBuild.length === 0) {
              log.warn("No services with 'build' configuration found", "build");
            } else {
              // Filter by service pattern if specified
              let filteredServices = servicesToBuild;
              if (deployOptions.services) {
                filteredServices = filterServicesByPatterns(
                  servicesToBuild,
                  deployOptions.services,
                  config!,
                );
              }

              log.info(
                `Building ${filteredServices.length} service(s): ${
                  filteredServices.map((s) => s.name).join(", ")
                }`,
                "build",
              );

              // Determine version tag
              const versionTag = await VersionManager.determineVersionTag({
                customVersion: globalOptions.version,
                useGitSha: true,
                shortSha: true,
              });

              // Set up registry if needed
              const registry = config!.builder.registry;
              let registryManager: RegistryManager | undefined;

              if (registry.isLocal()) {
                // Start local registry
                await log.group("Local Registry Setup", async () => {
                  registryManager = new RegistryManager(
                    config!.builder.engine,
                    registry.port,
                  );
                  await registryManager.setupForBuild();
                });
              }

              // Create BuildService and build all services
              const buildService = new BuildService({
                engine: config!.builder.engine,
                registry,
                globalOptions,
                noCache: deployOptions.noCache,
                push: true, // Deploy always pushes images
                cacheEnabled: config!.builder.cache,
              });

              await buildService.buildServices(filteredServices, versionTag);

              // Build summary
              log.success(
                `Successfully built ${filteredServices.length} service(s)`,
                "build",
              );
              log.info(`Version tag: ${versionTag}`, "build");
              log.info(`Registry: ${registry.getRegistryUrl()}`, "build");
            }
          });
        }

        // Collect all unique hosts
        const allHosts = config.getAllServerHosts();
        uniqueHosts = allHosts;

        if (uniqueHosts.length === 0) {
          log.error(
            `No remote hosts found in configuration at: ${configPath}`,
            "config",
          );
          Deno.exit(1);
        }

        log.info(
          `Found ${uniqueHosts.length} remote host(s): ${
            uniqueHosts.join(", ")
          }`,
          "deploy",
        );

        await log.group("SSH Connection Setup", async () => {
          const result = await setupSSHConnections(
            uniqueHosts,
            {
              user: config!.ssh.user,
              port: config!.ssh.port,
              proxy: config!.ssh.proxy,
              proxy_command: config!.ssh.proxyCommand,
              keys: config!.ssh.allKeys.length > 0
                ? config!.ssh.allKeys
                : undefined,
              keyData: config!.ssh.keyData,
              keysOnly: config!.ssh.keysOnly,
              dnsRetries: config!.ssh.dnsRetries,
            },
            { allowPartialConnection: true },
          );

          sshManagers = result.managers;
          uniqueHosts = result.connectedHosts;
        });

        // Set up port forwarding for local registry if needed
        if (config!.builder.registry.isLocal()) {
          await log.group("Local Registry Port Forwarding", async () => {
            const registryPort = config!.builder.registry.port;
            log.info(
              `Setting up reverse port forwarding for local registry (port ${registryPort})`,
              "registry",
            );

            portForwardManager = new PortForwardManager();

            // Set up port forwarding for each connected host
            for (const ssh of sshManagers!) {
              const host = ssh.getHost();
              log.status(
                `Establishing port forward to ${host}`,
                "port-forward",
              );

              const forwarder = portForwardManager.getForwarder(
                host,
                ssh,
                registryPort,
                registryPort,
              );

              try {
                await forwarder.startForwarding();
                log.success(
                  `Port forwarding active for ${host}`,
                  "port-forward",
                );
              } catch (error) {
                log.warn(
                  `Failed to set up port forwarding for ${host}: ${
                    error instanceof Error ? error.message : String(error)
                  }`,
                  "port-forward",
                );
              }
            }

            log.info(
              `Local registry accessible on remote hosts via localhost:${registryPort}`,
              "registry",
            );
          });
        }

        // Get deployable services and apply filtering if specified
        let allServices = config.getDeployableServices();

        // Apply service filtering if specified
        if (deployOptions.services) {
          allServices = filterServicesByPatterns(
            allServices,
            deployOptions.services,
            config,
          );

          log.info(
            `Deploying ${allServices.length} service(s): ${
              allServices.map((s) => s.name).join(", ")
            }`,
            "deploy",
          );
        } else {
          log.info(
            `Deploying all ${allServices.length} service(s): ${
              allServices.map((s) => s.name).join(", ")
            }`,
            "deploy",
          );
        }

        // Get services that need proxy configuration
        const servicesWithProxy = allServices.filter((service) =>
          service.proxy?.enabled
        );

        // Install proxy if any services need it
        if (servicesWithProxy.length > 0) {
          await log.group("Proxy Installation", async () => {
            log.info(
              `Found ${servicesWithProxy.length} service(s) with proxy configuration`,
              "proxy",
            );

            // Get unique hosts that need proxy
            const proxyHosts = new Set<string>();
            for (const service of servicesWithProxy) {
              for (const server of service.servers) {
                const host = server.host;
                if (uniqueHosts.includes(host)) {
                  proxyHosts.add(host);
                }
              }
            }

            log.info(
              `Installing kamal-proxy on ${proxyHosts.size} host(s): ${
                Array.from(proxyHosts).join(", ")
              }`,
              "proxy",
            );

            // Install proxy on each host that needs it
            const proxyResults: Array<{
              host: string;
              success: boolean;
              message?: string;
            }> = [];

            for (const host of proxyHosts) {
              const hostSsh = sshManagers!.find((ssh) =>
                ssh.getHost() === host
              );
              if (!hostSsh) continue;

              try {
                const proxyCmd = new ProxyCommands(
                  config!.builder.engine,
                  hostSsh,
                );

                // Ensure network exists
                await proxyCmd.ensureNetwork();

                // Check if proxy is already running
                const isRunning = await proxyCmd.isRunning();

                if (isRunning) {
                  const version = await proxyCmd.getVersion();
                  log.info(
                    `kamal-proxy already running on ${host} (version: ${
                      version || "unknown"
                    })`,
                    "proxy",
                  );
                  proxyResults.push({
                    host,
                    success: true,
                    message: "Already running",
                  });
                } else {
                  // Load network topology to get DNS server IP
                  let dnsServer: string | undefined;
                  if (config!.network.enabled) {
                    const topology = await loadTopology(hostSsh);
                    if (topology) {
                      const server = getServerByHostname(topology, host);
                      if (server) {
                        dnsServer = server.wireguardIp;
                      }
                    }
                  }

                  // Boot the proxy
                  log.status(`Booting kamal-proxy on ${host}...`, "proxy");
                  await proxyCmd.boot({ dnsServer });

                  const version = await proxyCmd.getVersion();
                  log.success(
                    `kamal-proxy started on ${host} (version: ${
                      version || "unknown"
                    })`,
                    "proxy",
                  );
                  proxyResults.push({
                    host,
                    success: true,
                    message: "Started",
                  });

                  // Log to audit
                  const hostLogger = createServerAuditLogger(
                    hostSsh,
                    config!.project,
                  );
                  await hostLogger.logProxyEvent(
                    "boot",
                    "success",
                    `kamal-proxy ${version || "unknown"} started`,
                  );
                }
              } catch (error) {
                const errorMessage = error instanceof Error
                  ? error.message
                  : String(error);
                log.error(
                  `Failed to install proxy on ${host}: ${errorMessage}`,
                  "proxy",
                );
                proxyResults.push({
                  host,
                  success: false,
                  message: errorMessage,
                });

                // Log failure to audit
                const hostLogger = createServerAuditLogger(
                  hostSsh,
                  config!.project,
                );
                await hostLogger.logProxyEvent(
                  "boot",
                  "failed",
                  errorMessage,
                );
              }
            }

            // Summary
            const successCount = proxyResults.filter((r) => r.success).length;
            const failCount = proxyResults.filter((r) => !r.success).length;

            if (successCount > 0) {
              log.success(
                `kamal-proxy ready on ${successCount} host(s)`,
                "proxy",
              );
            }
            if (failCount > 0) {
              log.error(
                `kamal-proxy installation failed on ${failCount} host(s)`,
                "proxy",
              );
            }
          });

          // Deploy service containers (both with and without proxy)
          await log.group("Service Container Deployment", async () => {
            for (const service of allServices) {
              log.status(`Deploying ${service.name} containers`, "deploy");

              for (const server of service.servers) {
                const host = server.host;
                if (!uniqueHosts.includes(host)) {
                  log.warn(
                    `Skipping ${service.name} on unreachable host: ${host}`,
                    "deploy",
                  );
                  continue;
                }

                const hostSsh = sshManagers!.find((ssh) =>
                  ssh.getHost() === host
                );
                if (!hostSsh) continue;

                try {
                  const containerName = service.getContainerName();

                  // Determine image name with optional version and registry
                  const version = globalOptions.version || "latest";
                  const imageName = service.requiresBuild()
                    ? config!.builder.registry.getFullImageName(
                      service.project,
                      service.name,
                      version,
                    )
                    : service.getImageName(undefined, version);

                  // Prepare files and directories before deployment
                  if (service.files.length > 0) {
                    log.status(
                      `Uploading ${service.files.length} file(s) for ${service.name} on ${host}`,
                      "deploy",
                    );
                    try {
                      await prepareMountFiles(
                        hostSsh,
                        service.files,
                        config!.project,
                      );
                      log.success(
                        `Files uploaded for ${service.name} on ${host}`,
                        "deploy",
                      );
                    } catch (error) {
                      log.error(
                        `Failed to upload files: ${
                          error instanceof Error ? error.message : String(error)
                        }`,
                        "deploy",
                      );
                      throw error;
                    }
                  }

                  if (service.directories.length > 0) {
                    log.status(
                      `Creating ${service.directories.length} director(ies) for ${service.name} on ${host}`,
                      "deploy",
                    );
                    try {
                      await prepareMountDirectories(
                        hostSsh,
                        service.directories,
                        config!.project,
                      );
                      log.success(
                        `Directories created for ${service.name} on ${host}`,
                        "deploy",
                      );
                    } catch (error) {
                      log.error(
                        `Failed to create directories: ${
                          error instanceof Error ? error.message : String(error)
                        }`,
                        "deploy",
                      );
                      throw error;
                    }
                  }

                  // For Podman, ensure image has full registry path
                  const fullImageName = imageName.includes("/")
                    ? imageName
                    : `docker.io/library/${imageName}`;

                  // Authenticate to registry on remote server if using remote registry
                  if (
                    !config!.builder.registry.isLocal() &&
                    service.requiresBuild()
                  ) {
                    const registryUrl = config!.builder.registry
                      .getRegistryUrl();
                    const username = config!.builder.registry.username;
                    const password = config!.builder.registry.password;

                    if (username && password) {
                      log.status(
                        `Authenticating to ${registryUrl} on ${host}`,
                        "deploy",
                      );

                      // Use echo to pipe password to podman/docker login
                      const loginCommand = `echo '${password}' | ${
                        config!.builder.engine
                      } login ${registryUrl} --username ${username} --password-stdin`;
                      const loginResult = await hostSsh.executeCommand(
                        loginCommand,
                      );

                      if (!loginResult.success) {
                        log.warn(
                          `Failed to authenticate to ${registryUrl} on ${host}: ${loginResult.stderr}`,
                          "deploy",
                        );
                      } else {
                        log.success(
                          `Authenticated to ${registryUrl} on ${host}`,
                          "deploy",
                        );
                      }
                    }
                  }

                  // Build pull command with TLS verification disabled for local registries
                  let pullCommand = `${config!.builder.engine} pull`;

                  // Add --tls-verify=false for local registries when using podman
                  if (
                    config!.builder.registry.isLocal() &&
                    config!.builder.engine === "podman"
                  ) {
                    pullCommand += " --tls-verify=false";
                  }

                  pullCommand += ` ${fullImageName}`;

                  // Pull image
                  log.status(
                    `Pulling image ${fullImageName} on ${host}`,
                    "deploy",
                  );
                  const pullResult = await hostSsh.executeCommand(pullCommand);
                  if (!pullResult.success) {
                    throw new Error(
                      `Failed to pull image: ${pullResult.stderr}`,
                    );
                  }

                  // Clean up old service containers from network registry
                  if (config!.network.enabled) {
                    try {
                      const cleanedCount = await cleanupServiceContainers(
                        hostSsh,
                        service.name,
                        config!.builder.engine,
                        config!.project,
                      );
                      if (cleanedCount > 0) {
                        log.debug(
                          `Cleaned up ${cleanedCount} stale containers for ${service.name}`,
                          "network",
                        );
                      }
                    } catch (error) {
                      log.warn(
                        `Service cleanup failed: ${error} (deployment will continue)`,
                        "network",
                      );
                    }
                  }

                  // Stop and remove existing container
                  await hostSsh.executeCommand(
                    `${
                      config!.builder.engine
                    } rm -f ${containerName} 2>/dev/null || true`,
                  );

                  // Build container run command
                  const mountArgs = buildAllMountArgs(
                    service.files,
                    service.directories,
                    service.volumes,
                    config!.project,
                  );
                  const mergedEnv = service.getMergedEnvironment();
                  const envArray = mergedEnv.toEnvArray();

                  // Get DNS server from network topology
                  let dnsServer: string | undefined;
                  if (config!.network.enabled) {
                    const topology = await loadTopology(hostSsh);
                    if (topology) {
                      const server = getServerByHostname(topology, host);
                      if (server) {
                        dnsServer = server.wireguardIp;
                      }
                    }
                  }

                  const builder = new ContainerRunBuilder(
                    config!.builder.engine,
                    containerName,
                    fullImageName,
                  )
                    .network("jiji")
                    .detached()
                    .restart("unless-stopped")
                    .ports(service.ports)
                    .volumes(mountArgs)
                    .environment(envArray);

                  // Add DNS configuration if network is enabled
                  if (dnsServer) {
                    builder.dns(dnsServer, config!.network.serviceDomain);
                  }

                  const runCommand = builder.build();

                  log.status(
                    `Starting container ${containerName} on ${host}`,
                    "deploy",
                  );
                  const runResult = await hostSsh.executeCommand(runCommand);
                  if (!runResult.success) {
                    throw new Error(
                      `Failed to start container: ${runResult.stderr}`,
                    );
                  }

                  // Wait for container to be running
                  let attempts = 0;
                  const maxAttempts = 10;
                  while (attempts < maxAttempts) {
                    const statusResult = await hostSsh.executeCommand(
                      `${
                        config!.builder.engine
                      } inspect ${containerName} --format '{{.State.Status}}'`,
                    );
                    if (
                      statusResult.success &&
                      statusResult.stdout.trim() === "running"
                    ) {
                      break;
                    }
                    attempts++;
                    await new Promise((resolve) => setTimeout(resolve, 1000));
                  }

                  if (attempts >= maxAttempts) {
                    throw new Error(
                      `Container ${containerName} did not start within ${maxAttempts} seconds`,
                    );
                  }

                  // Register container in network (if enabled)
                  if (config!.network.enabled) {
                    try {
                      // Load topology from Corrosion via SSH
                      const topology = await loadTopology(hostSsh);
                      if (topology) {
                        const server = getServerByHostname(topology, host);
                        if (server) {
                          log.status(
                            `Registering ${service.name} in network...`,
                            "network",
                          );

                          // First register locally (this gets IP and sets up DNS)
                          const registered = await registerContainerInNetwork(
                            hostSsh,
                            service.name,
                            config!.project,
                            server.id,
                            containerName,
                            config!.builder.engine,
                          );

                          if (registered) {
                            // Get container IP for cluster-wide registration
                            const { getContainerIp } = await import(
                              "../lib/services/container_registry.ts"
                            );
                            const containerIp = await getContainerIp(
                              hostSsh,
                              containerName,
                              config!.builder.engine,
                            );

                            if (containerIp && sshManagers) {
                              // Register this container on all servers for DNS resolution
                              await registerContainerClusterWide(
                                sshManagers,
                                service.name,
                                config!.project,
                                server.id,
                                containerName,
                                containerIp,
                                Date.now(),
                              );
                              log.debug(
                                `Registered ${service.name} cluster-wide for DNS resolution`,
                                "network",
                              );
                            }
                          } else {
                            log.warn(
                              `Failed to register ${service.name} in network (service will still run)`,
                              "network",
                            );
                          }
                        } else {
                          log.warn(
                            `Server ${host} not found in network topology`,
                            "network",
                          );
                        }
                      } else {
                        log.warn(
                          `Network cluster not initialized - skipping network registration`,
                          "network",
                        );
                      }
                    } catch (error) {
                      log.warn(
                        `Network registration failed: ${error} (service will still run)`,
                        "network",
                      );
                    }
                  }

                  log.success(
                    `${service.name} deployed successfully on ${host}`,
                    "deploy",
                  );
                } catch (error) {
                  log.error(
                    `Failed to deploy ${service.name} on ${host}: ${
                      error instanceof Error ? error.message : String(error)
                    }`,
                    "deploy",
                  );
                }
              }
            }
          });

          // Configure proxy for services that need it
          if (servicesWithProxy.length > 0) {
            await log.group("Service Proxy Configuration", async () => {
              for (const service of servicesWithProxy) {
                log.status(
                  `Configuring proxy for service: ${service.name}`,
                  "proxy",
                );

                const proxyConfig = service.proxy!;
                const appPort = extractAppPort(service.ports);

                for (const server of service.servers) {
                  const host = server.host;
                  if (!uniqueHosts.includes(host)) {
                    log.warn(
                      `Skipping ${service.name} on unreachable host: ${host}`,
                      "proxy",
                    );
                    continue;
                  }

                  const hostSsh = sshManagers!.find((ssh) =>
                    ssh.getHost() === host
                  );
                  if (!hostSsh) continue;

                  try {
                    const proxyCmd = new ProxyCommands(
                      config!.builder.engine,
                      hostSsh,
                    );
                    const containerName = service.getContainerName();

                    await proxyCmd.deploy(
                      service.name,
                      containerName,
                      proxyConfig,
                      appPort,
                      config!.project,
                    );

                    log.success(
                      `${service.name} configured on proxy at ${host} (${proxyConfig.host}, port ${appPort})`,
                      "proxy",
                    );

                    // Log to audit
                    const hostLogger = createServerAuditLogger(
                      hostSsh,
                      config!.project,
                    );
                    await hostLogger.logProxyEvent(
                      "deploy",
                      "success",
                      `${service.name} -> ${proxyConfig.host}:${appPort} (SSL: ${proxyConfig.ssl})`,
                    );
                  } catch (error) {
                    const errorMessage = error instanceof Error
                      ? error.message
                      : String(error);
                    log.error(
                      `Failed to configure ${service.name} on proxy at ${host}: ${errorMessage}`,
                      "proxy",
                    );

                    // Fetch and display service container logs to help diagnose the issue
                    const containerName = service.getContainerName();
                    log.info(
                      `Fetching logs from ${containerName} to diagnose the issue...`,
                      "proxy",
                    );

                    const logsCmd = `${
                      config!.builder.engine
                    } logs --tail 50 ${containerName} 2>&1`;
                    const logsResult = await hostSsh.executeCommand(logsCmd);

                    if (logsResult.success && logsResult.stdout.trim()) {
                      log.error(
                        `Container logs for ${containerName}:`,
                        "proxy",
                      );
                      // Split logs into lines and display each with error level
                      const logLines = logsResult.stdout.trim().split("\n");
                      for (const line of logLines) {
                        log.error(`  ${line}`, "proxy");
                      }
                    } else {
                      log.warn(
                        `No logs available for ${containerName}`,
                        "proxy",
                      );
                    }
                  }
                }
              }
            });
          }
        } else if (allServices.length === 0) {
          log.info("No services found to deploy", "deploy");
        }

        log.success("Deployment process completed", "deploy");
      });
    } catch (error) {
      const errorMessage = error instanceof Error
        ? error.message
        : String(error);
      log.error("Deployment failed:", "deploy");
      log.error(errorMessage, "deploy");
      Deno.exit(1);
    } finally {
      // Clean up port forwarding
      if (portForwardManager) {
        try {
          await portForwardManager.cleanup();
        } catch (error) {
          log.debug(
            `Failed to cleanup port forwarding: ${
              error instanceof Error ? error.message : String(error)
            }`,
            "port-forward",
          );
        }
      }

      // Clean up SSH connections
      if (sshManagers) {
        sshManagers.forEach((ssh) => {
          try {
            ssh.dispose();
          } catch (error) {
            log.debug(`Failed to dispose SSH connection: ${error}`, "ssh");
          }
        });
      }
    }
  });
