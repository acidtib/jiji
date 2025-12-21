import { Command } from "@cliffy/command";
import { Confirm } from "@cliffy/prompt";
import { Configuration } from "../lib/configuration.ts";
import { setupCommandContext } from "../utils/command_helpers.ts";
import { handleCommandError } from "../utils/error_handler.ts";
import { log } from "../utils/logger.ts";
import { PortForwardManager } from "../utils/port_forward.ts";
import { VersionManager } from "../utils/version_manager.ts";
import { RegistryManager } from "../utils/registry_manager.ts";
import { filterServicesByPatterns } from "../utils/config.ts";
import { BuildService } from "../lib/services/build_service.ts";
import { ProxyService } from "../lib/services/proxy_service.ts";
import { ContainerDeploymentService } from "../lib/services/container_deployment_service.ts";
import { ImagePruneService } from "../lib/services/image_prune_service.ts";

import type { GlobalOptions } from "../types.ts";

interface DeployOptions extends GlobalOptions {
  build?: boolean;
  noCache?: boolean;
  yes?: boolean;
}

/**
 * Display deployment plan and get user confirmation
 */
async function displayDeploymentPlan(
  config: Configuration,
  deployOptions: DeployOptions,
  globalOptions: GlobalOptions,
): Promise<boolean> {
  await log.group("Deployment Plan", () => {
    log.info("Analyzing deployment configuration...", "plan");

    // Get services that will be deployed
    let allServices = config.getDeployableServices();

    // Apply service filtering if specified
    if (deployOptions.services) {
      allServices = filterServicesByPatterns(
        allServices,
        deployOptions.services,
        config,
      );
    }

    // Get services that will be built
    let buildServices = config.getBuildServices();
    if (deployOptions.services) {
      buildServices = filterServicesByPatterns(
        buildServices,
        deployOptions.services,
        config,
      );
    }

    // Display project information
    console.log("\nDeployment Plan");
    console.log("═".repeat(50));
    console.log(`Project: ${config.project}`);
    console.log(`Container Engine: ${config.builder.engine}`);
    console.log(`Registry: ${config.builder.registry.getRegistryUrl()}`);

    if (globalOptions.version) {
      console.log(`Version: ${globalOptions.version}`);
    }

    // Display build information if --build flag is set
    if (deployOptions.build && buildServices.length > 0) {
      console.log("\nServices to Build:");
      for (const service of buildServices) {
        console.log(`  • ${service.name}`);
        if (typeof service.build === "string") {
          console.log(`    Context: ${service.build}`);
        } else if (service.build) {
          console.log(`    Context: ${service.build.context}`);
          if (service.build.dockerfile) {
            console.log(`    Dockerfile: ${service.build.dockerfile}`);
          }
          if (service.build.target) {
            console.log(`    Target: ${service.build.target}`);
          }
        }
      }
    }

    // Display deployment information
    console.log("\nServices to Deploy:");
    if (allServices.length === 0) {
      console.log("  No services to deploy");
    } else {
      for (const service of allServices) {
        console.log(`  • ${service.name}`);

        // Show image or build source
        if (service.image) {
          console.log(`    Image: ${service.image}`);
        } else if (service.build) {
          console.log(
            `    Built from: ${
              typeof service.build === "string"
                ? service.build
                : service.build.context
            }`,
          );
        }

        // Show target servers
        if (service.servers.length > 0) {
          console.log(
            `    Servers: ${service.servers.map((s) => s.host).join(", ")}`,
          );
        }

        // Show ports if any
        if (service.ports.length > 0) {
          console.log(`    Ports: ${service.ports.join(", ")}`);
        }

        // Show proxy info
        if (service.proxy?.enabled) {
          const hosts = service.proxy.hosts.length > 0
            ? service.proxy.hosts.join(", ")
            : "auto";
          console.log(`    Proxy: Enabled (${hosts})`);
        }
      }
    }

    // Show additional options
    const options: string[] = [];
    if (deployOptions.build) options.push("Build images");
    if (deployOptions.noCache) options.push("No cache");
    if (globalOptions.hosts) {
      options.push(`Target hosts: ${globalOptions.hosts}`);
    }

    if (options.length > 0) {
      console.log("\nOptions:");
      options.forEach((option) => console.log(`  • ${option}`));
    }

    console.log("═".repeat(50));
  });

  // Skip confirmation if --yes flag is provided
  if (deployOptions.yes) {
    log.info("Skipping confirmation (--yes flag provided)", "plan");
    return true;
  }

  // Get user confirmation
  console.log();
  const confirmed = await Confirm.prompt({
    message: "Do you want to proceed with this deployment?",
    default: false,
  });

  return confirmed;
}

export const deployCommand = new Command()
  .description("Deploy services to servers")
  .option("--build", "Build images before deploying", { default: false })
  .option("--no-cache", "Build without using cache (requires --build)")
  .option("-y, --yes", "Skip deployment confirmation prompt", {
    default: false,
  })
  .action(async (options) => {
    const deployOptions = options as unknown as DeployOptions;
    const globalOptions = options as unknown as GlobalOptions;
    let ctx: Awaited<ReturnType<typeof setupCommandContext>> | undefined;
    let portForwardManager: PortForwardManager | undefined;

    try {
      await log.group("Service Deployment", async () => {
        log.info("Starting service deployment process", "deploy");

        // Load configuration first (without SSH for build phase)
        const config = await Configuration.load(
          globalOptions.environment,
          globalOptions.configFile,
        );
        log.success(
          `Configuration loaded from: ${config.configPath || "unknown"}`,
          "config",
        );
        log.info(`Container engine: ${config.builder.engine}`, "engine");

        // Display deployment plan and get confirmation
        const shouldProceed = await displayDeploymentPlan(
          config,
          deployOptions,
          globalOptions,
        );
        if (!shouldProceed) {
          log.info("Deployment cancelled by user", "deploy");
          return;
        }

        // Build images if --build flag is set
        if (deployOptions.build) {
          await log.group("Service Build", async () => {
            log.info("Building service images", "build");

            // Get services to build
            const servicesToBuild = config.getBuildServices();

            if (servicesToBuild.length === 0) {
              log.warn("No services with 'build' configuration found", "build");
            } else {
              // Filter by service pattern if specified
              let filteredServices = servicesToBuild;
              if (deployOptions.services) {
                filteredServices = filterServicesByPatterns(
                  servicesToBuild,
                  deployOptions.services,
                  config,
                );
              }

              log.info(
                `Building ${filteredServices.length} service(s): ${
                  filteredServices.map((s) => s.name).join(", ")
                }`,
                "build",
              );

              // Determine version tag for build services
              // Build services should use: --version > git SHA > ULID
              const versionTag = await VersionManager.determineVersionTag({
                customVersion: globalOptions.version,
                useGitSha: true,
                shortSha: true,
                isImageService: false, // These are build services
                serviceName: filteredServices.map((s) => s.name).join(", "),
              });

              // Set up registry if needed
              const registry = config.builder.registry;
              let registryManager: RegistryManager | undefined;

              if (registry.isLocal()) {
                // Start local registry
                await log.group("Local Registry Setup", async () => {
                  registryManager = new RegistryManager(
                    config.builder.engine,
                    registry.port,
                  );
                  await registryManager.setupForBuild();
                });
              }

              // Create BuildService and build all services
              const buildService = new BuildService({
                engine: config.builder.engine,
                registry,
                globalOptions,
                noCache: deployOptions.noCache,
                push: true, // Deploy always pushes images
                cacheEnabled: config.builder.cache,
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

        // Now set up SSH connections for deployment
        ctx = await setupCommandContext(globalOptions);
        const { sshManagers, targetHosts } = ctx;

        // Set up port forwarding for local registry if needed
        if (config.builder.registry.isLocal()) {
          await log.group("Local Registry Port Forwarding", async () => {
            const registryPort = config.builder.registry.port;
            log.info(
              `Setting up reverse port forwarding for local registry (port ${registryPort})`,
              "registry",
            );

            portForwardManager = new PortForwardManager();

            // Set up port forwarding for each connected host
            for (const ssh of sshManagers) {
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

            // Create ProxyService and ensure proxy on hosts
            const proxyService = new ProxyService(
              config.builder.engine,
              config,
              sshManagers,
            );

            const proxyHosts = ProxyService.getHostsNeedingProxy(
              servicesWithProxy,
              targetHosts,
            );

            await proxyService.ensureProxyOnHosts(proxyHosts);
          });
        }

        // Deploy service containers
        if (allServices.length > 0) {
          await log.group("Service Container Deployment", async () => {
            const deploymentService = new ContainerDeploymentService(
              config.builder.engine,
              config,
            );

            await deploymentService.deployServices(
              allServices,
              sshManagers,
              targetHosts,
              {
                version: globalOptions.version,
              },
            );
          });

          // Configure proxy for services that need it
          if (servicesWithProxy.length > 0) {
            await log.group("Service Proxy Configuration", async () => {
              const proxyService = new ProxyService(
                config.builder.engine,
                config,
                sshManagers,
              );

              await proxyService.configureProxyForServices(servicesWithProxy);
            });
          }

          // Prune old images after deployment
          await log.group("Image Cleanup", async () => {
            log.info(
              "Pruning old images to retain configured versions",
              "prune",
            );

            const pruneService = new ImagePruneService(
              config.builder.engine,
              config.project,
            );

            // Prune images on each host, using the maximum retain value from all services
            const maxRetain = Math.max(...allServices.map((s) => s.retain));
            log.info(
              `Retaining up to ${maxRetain} image(s) per service`,
              "prune",
            );

            const pruneResults = await pruneService.pruneImagesOnHosts(
              sshManagers,
              {
                retain: maxRetain,
                removeDangling: true,
              },
            );

            const totalRemoved = pruneResults.reduce(
              (sum, r) => sum + r.imagesRemoved,
              0,
            );

            if (totalRemoved > 0) {
              log.success(
                `Pruned ${totalRemoved} old image(s) across ${pruneResults.length} server(s)`,
                "prune",
              );
            } else {
              log.info("No old images to prune", "prune");
            }
          });
        } else {
          log.info("No services found to deploy", "deploy");
        }

        log.success("Deployment process completed", "deploy");
      });
    } catch (error) {
      await handleCommandError(error, {
        operation: "Deployment",
        component: "deploy",
        sshManagers: ctx?.sshManagers,
        projectName: ctx?.config?.project,
        targetHosts: ctx?.targetHosts,
      });
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

      // SSH cleanup
      if (ctx?.sshManagers) {
        const { cleanupSSHConnections } = await import(
          "../utils/command_helpers.ts"
        );
        cleanupSSHConnections(ctx.sshManagers);
      }
    }
  });
