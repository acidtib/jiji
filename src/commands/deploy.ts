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
import { GitUtils } from "../utils/git.ts";
import { RegistryManager } from "../utils/registry_manager.ts";

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
                const servicePatterns = deployOptions.services.split(",").map((
                  s,
                ) => s.trim());
                const matchingNames = config!.getMatchingServiceNames(
                  servicePatterns,
                );
                filteredServices = servicesToBuild.filter((service) =>
                  matchingNames.includes(service.name)
                );

                if (filteredServices.length === 0) {
                  log.error(
                    `No buildable services match pattern: ${deployOptions.services}`,
                    "build",
                  );
                  Deno.exit(1);
                }
              }

              log.info(
                `Building ${filteredServices.length} service(s): ${
                  filteredServices.map((s) => s.name).join(", ")
                }`,
                "build",
              );

              // Determine version tag
              let versionTag: string;
              if (globalOptions.version) {
                versionTag = globalOptions.version;
                log.info(`Using custom version tag: ${versionTag}`, "build");
              } else {
                // Check if we're in a git repository
                if (!await GitUtils.isGitRepository()) {
                  log.error(
                    "Not in a git repository. Either initialize git or use --version to specify a tag.",
                    "build",
                  );
                  Deno.exit(1);
                }

                // Get git SHA
                versionTag = await GitUtils.getCommitSHA(true);
                log.info(`Using git SHA as version: ${versionTag}`, "build");

                // Warn about uncommitted changes
                if (await GitUtils.hasUncommittedChanges()) {
                  log.warn(
                    "You have uncommitted changes. The build will be tagged with the current commit SHA.",
                    "build",
                  );
                }
              }

              // Set up registry if needed
              const registry = config!.builder.registry;
              const registryUrl = registry.getRegistryUrl();
              let registryManager: RegistryManager | undefined;

              if (registry.isLocal()) {
                // Start local registry
                await log.group("Local Registry Setup", async () => {
                  registryManager = new RegistryManager(
                    config!.builder.engine,
                    registry.port,
                  );

                  if (!await registryManager.isRunning()) {
                    log.info("Starting local registry", "registry");
                    await registryManager.start();
                  } else {
                    log.info(
                      `Local registry already running on port ${registry.port}`,
                      "registry",
                    );
                  }
                });
              }

              // Build each service
              for (const service of filteredServices) {
                await log.group(`Building: ${service.name}`, async () => {
                  const buildConfig = typeof service.build === "string"
                    ? {
                      context: service.build,
                      dockerfile: "Dockerfile",
                      args: {},
                    }
                    : service.build!;

                  const context = buildConfig.context;
                  const dockerfile = buildConfig.dockerfile || "Dockerfile";
                  const buildArgs = buildConfig.args || {};
                  const target = buildConfig.target;

                  // Get architectures required by servers
                  const requiredArchs = service.getRequiredArchitectures();
                  const serversByArch = service.getServersByArchitecture();

                  // Build the image
                  const imageName = service.getImageName(
                    registryUrl,
                    versionTag,
                  );
                  const latestImageName = service.getImageName(
                    registryUrl,
                    "latest",
                  );

                  log.info(`Building image: ${imageName}`, "build");
                  log.info(`Context: ${context}`, "build");
                  log.info(`Dockerfile: ${dockerfile}`, "build");
                  log.info(
                    `Architecture(s): ${requiredArchs.join(", ")}`,
                    "build",
                  );

                  // Log server distribution by architecture
                  for (const [arch, servers] of serversByArch.entries()) {
                    log.info(`${arch}: ${servers.join(", ")}`, "build");
                  }

                  // Construct build command
                  const buildCmdArgs = [
                    "build",
                    "-t",
                    imageName,
                    "-t",
                    latestImageName,
                    "-f",
                    dockerfile,
                  ];

                  // Add build args
                  for (const [key, value] of Object.entries(buildArgs)) {
                    buildCmdArgs.push("--build-arg", `${key}=${value}`);
                  }

                  // Add target if specified
                  if (target) {
                    buildCmdArgs.push("--target", target);
                  }

                  // Add platform/architecture based on server requirements
                  if (requiredArchs.length > 1) {
                    // Multiple architectures - use platforms for multi-arch build
                    const platforms = requiredArchs.map((a) => `linux/${a}`)
                      .join(",");
                    buildCmdArgs.push("--platform", platforms);
                  } else if (requiredArchs.length === 1) {
                    // Single architecture
                    buildCmdArgs.push(
                      "--platform",
                      `linux/${requiredArchs[0]}`,
                    );
                  }

                  // Add cache option
                  if (deployOptions.noCache || !config!.builder.cache) {
                    buildCmdArgs.push("--no-cache");
                  }

                  // Add context
                  buildCmdArgs.push(context);

                  // Execute build
                  const buildCmd = new Deno.Command(config!.builder.engine, {
                    args: buildCmdArgs,
                    stdout: globalOptions.verbose ? "inherit" : "piped",
                    stderr: globalOptions.verbose ? "inherit" : "piped",
                  });

                  const buildResult = await buildCmd.output();

                  if (buildResult.code !== 0) {
                    const stderr = new TextDecoder().decode(buildResult.stderr);
                    log.error(`Failed to build ${service.name}`, "build");
                    if (!globalOptions.verbose) {
                      log.error(stderr, "build");
                    }
                    throw new Error(
                      `Build failed for service: ${service.name}`,
                    );
                  }

                  log.success(`Built image: ${imageName}`, "build");
                  log.success(`Tagged as: ${latestImageName}`, "build");

                  // Push to registry
                  await log.group("Pushing to Registry", async () => {
                    log.info(`Pushing ${imageName}`, "registry");

                    // Build push arguments
                    const pushArgs = ["push"];

                    // Add --tls-verify=false for local registries when using podman
                    if (
                      registry.isLocal() && config!.builder.engine === "podman"
                    ) {
                      pushArgs.push("--tls-verify=false");
                    }

                    pushArgs.push(imageName);

                    // Push versioned image
                    const pushCmd = new Deno.Command(config!.builder.engine, {
                      args: pushArgs,
                      stdout: globalOptions.verbose ? "inherit" : "piped",
                      stderr: globalOptions.verbose ? "inherit" : "piped",
                    });

                    const pushResult = await pushCmd.output();

                    if (pushResult.code !== 0) {
                      const stderr = new TextDecoder().decode(
                        pushResult.stderr,
                      );
                      log.error(`Failed to push ${imageName}`, "registry");
                      if (!globalOptions.verbose) {
                        log.error(stderr, "registry");
                      }
                      throw new Error(`Push failed for: ${imageName}`);
                    }

                    log.success(`Pushed: ${imageName}`, "registry");

                    // Push latest tag
                    log.info(`Pushing ${latestImageName}`, "registry");

                    // Build push arguments for latest
                    const pushLatestArgs = ["push"];

                    // Add --tls-verify=false for local registries when using podman
                    if (
                      registry.isLocal() && config!.builder.engine === "podman"
                    ) {
                      pushLatestArgs.push("--tls-verify=false");
                    }

                    pushLatestArgs.push(latestImageName);

                    const pushLatestCmd = new Deno.Command(
                      config!.builder.engine,
                      {
                        args: pushLatestArgs,
                        stdout: globalOptions.verbose ? "inherit" : "piped",
                        stderr: globalOptions.verbose ? "inherit" : "piped",
                      },
                    );

                    const pushLatestResult = await pushLatestCmd.output();

                    if (pushLatestResult.code !== 0) {
                      const stderr = new TextDecoder().decode(
                        pushLatestResult.stderr,
                      );
                      log.error(
                        `Failed to push ${latestImageName}`,
                        "registry",
                      );
                      if (!globalOptions.verbose) {
                        log.error(stderr, "registry");
                      }
                      throw new Error(`Push failed for: ${latestImageName}`);
                    }

                    log.success(`Pushed: ${latestImageName}`, "registry");
                  });
                });
              }

              // Build summary
              log.success(
                `Successfully built ${filteredServices.length} service(s)`,
                "build",
              );
              log.info(`Version tag: ${versionTag}`, "build");
              log.info(`Registry: ${registryUrl}`, "build");
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

        // Check which services have proxy configuration
        const servicesWithProxy = Array.from(config.services.values())
          .filter((service) => service.proxy?.enabled);

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
                  // Boot the proxy
                  log.status(`Booting kamal-proxy on ${host}...`, "proxy");
                  await proxyCmd.boot();

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

          // Deploy service containers
          await log.group("Service Container Deployment", async () => {
            for (const service of servicesWithProxy) {
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
                  const version = globalOptions.version;
                  const registry = config!.builder.registry.getRegistryUrl();
                  const imageName = service.getImageName(registry, version);

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

                  // Stop and remove existing container
                  await hostSsh.executeCommand(
                    `${
                      config!.builder.engine
                    } rm -f ${containerName} 2>/dev/null || true`,
                  );

                  // Build container run command
                  const portArgs = service.ports
                    .map((p) => `-p ${p}`)
                    .join(" ");
                  const mountArgs = buildAllMountArgs(
                    service.files,
                    service.directories,
                    service.volumes,
                    config!.project,
                  );
                  const envArgs = service.environment
                    ? (Array.isArray(service.environment)
                      ? service.environment.map((e) => `-e "${e}"`).join(" ")
                      : Object.entries(service.environment)
                        .map(([k, v]) => `-e ${k}="${v}"`)
                        .join(" "))
                    : "";

                  const runCommand = `${
                    config!.builder.engine
                  } run --name ${containerName} --network jiji --detach --restart unless-stopped ${portArgs} ${mountArgs} ${envArgs} ${fullImageName}`;

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

          // Deploy services to proxy
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
                }
              }
            }
          });
        } else {
          log.info("No services configured with proxy", "deploy");
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
