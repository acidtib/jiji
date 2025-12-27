import { Command } from "@cliffy/command";
import { Confirm } from "@cliffy/prompt";
import { Configuration } from "../lib/configuration.ts";
import {
  cleanupSSHConnections,
  setupCommandContext,
} from "../utils/command_helpers.ts";
import { handleCommandError } from "../utils/error_handler.ts";
import { log } from "../utils/logger.ts";
import { PortForwardManager } from "../utils/port_forward.ts";
import { VersionManager } from "../utils/version_manager.ts";
import { RegistryManager } from "../utils/registry_manager.ts";
import { filterServicesByPatterns } from "../utils/config.ts";
import { BuildService } from "../lib/services/build_service.ts";
import { ImagePruneService } from "../lib/services/image_prune_service.ts";
import { DeploymentOrchestrator } from "../lib/services/deployment_orchestrator.ts";

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
  log.section("Deployment Plan:");

  let allServices = config.getDeployableServices();

  if (deployOptions.services) {
    allServices = filterServicesByPatterns(
      allServices,
      deployOptions.services,
      config,
    );
  }

  let buildServices = config.getBuildServices();
  if (deployOptions.services) {
    buildServices = filterServicesByPatterns(
      buildServices,
      deployOptions.services,
      config,
    );
  }

  log.say(`Project: ${config.project}`, 1);
  log.say(`Container Engine: ${config.builder.engine}`, 1);
  log.say(`Registry: ${config.builder.registry.getRegistryUrl()}`, 1);

  if (globalOptions.version) {
    log.say(`Version: ${globalOptions.version}`, 1);
  }

  if (deployOptions.build && buildServices.length > 0) {
    console.log();
    log.say("Services to Build:", 1);
    for (const service of buildServices) {
      log.say(`${service.name}`, 2);
      if (typeof service.build === "string") {
        log.say(`Context: ${service.build}`, 3);
      } else if (service.build) {
        log.say(`Context: ${service.build.context}`, 3);
        if (service.build.dockerfile) {
          log.say(`Dockerfile: ${service.build.dockerfile}`, 3);
        }
        if (service.build.target) {
          log.say(`Target: ${service.build.target}`, 3);
        }
      }
    }
  }

  console.log();
  log.say("Services to Deploy:", 1);
  if (allServices.length === 0) {
    log.say("No services to deploy", 2);
  } else {
    for (const service of allServices) {
      log.say(`${service.name}`, 2);

      if (service.image) {
        log.say(`Image: ${service.image}`, 3);
      } else if (service.build) {
        log.say(
          `Built from: ${
            typeof service.build === "string"
              ? service.build
              : service.build.context
          }`,
          3,
        );
      }

      if (service.servers.length > 0) {
        log.say(
          `Servers: ${service.servers.map((s) => s.host).join(", ")}`,
          3,
        );
      }

      if (service.ports.length > 0) {
        log.say(`Ports: ${service.ports.join(", ")}`, 3);
      }

      if (service.proxy?.enabled) {
        // Collect all hosts from all targets
        const allHosts: string[] = [];
        for (const target of service.proxy.targets) {
          if (target.hosts) {
            allHosts.push(...target.hosts);
          } else if (target.host) {
            allHosts.push(target.host);
          }
        }
        const hosts = allHosts.length > 0 ? allHosts.join(", ") : "auto";
        log.say(`Proxy: Enabled (${hosts})`, 3);
      }
    }
  }

  const options: string[] = [];
  if (deployOptions.build) options.push("Build images");
  if (deployOptions.noCache) options.push("No cache");
  if (globalOptions.hosts) {
    options.push(`Target hosts: ${globalOptions.hosts}`);
  }

  if (options.length > 0) {
    console.log();
    log.say("Options:", 1);
    options.forEach((option) => log.say(`${option}`, 2));
  }

  console.log();

  if (deployOptions.yes) {
    log.say("Proceeding with deployment (--yes flag provided)", 1);
    return true;
  }

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
      log.section("Service Deployment:");

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

      const shouldProceed = await displayDeploymentPlan(
        config,
        deployOptions,
        globalOptions,
      );
      if (!shouldProceed) {
        log.say("Deployment cancelled by user");
        return;
      }

      if (deployOptions.build) {
        log.section("Building Service Images:");

        const servicesToBuild = config.getBuildServices();

        if (servicesToBuild.length === 0) {
          log.say("No services with 'build' configuration found", 1);
        } else {
          let filteredServices = servicesToBuild;
          if (deployOptions.services) {
            filteredServices = filterServicesByPatterns(
              servicesToBuild,
              deployOptions.services,
              config,
            );
          }

          log.say(
            `Building ${filteredServices.length} service(s): ${
              filteredServices.map((s) => s.name).join(", ")
            }`,
            1,
          );

          const versionTag = await VersionManager.determineVersionTag({
            customVersion: globalOptions.version,
            useGitSha: true,
            shortSha: true,
            isImageService: false,
            serviceName: filteredServices.map((s) => s.name).join(", "),
          });

          const registry = config.builder.registry;
          let registryManager: RegistryManager | undefined;

          if (registry.isLocal()) {
            log.say("Setting up local registry...", 1);
            registryManager = new RegistryManager(
              config.builder.engine,
              registry.port,
            );
            await registryManager.setupForBuild();
          }

          const buildService = new BuildService({
            engine: config.builder.engine,
            registry,
            globalOptions,
            noCache: deployOptions.noCache,
            push: true,
            cacheEnabled: config.builder.cache,
          });

          await buildService.buildServices(filteredServices, versionTag);

          log.say(`Version tag: ${versionTag}`, 1);
          log.say(`Registry: ${registry.getRegistryUrl()}`, 1);
        }
      }

      ctx = await setupCommandContext(globalOptions);
      const { sshManagers, targetHosts } = ctx;

      // Show connection status for each host
      console.log(""); // Empty line
      for (const ssh of sshManagers) {
        log.remote(ssh.getHost(), ": Connected", { indent: 1 });
      }

      if (config.builder.registry.isLocal()) {
        log.section("Setting Up Local Registry Access:");

        const registryPort = config.builder.registry.port;
        portForwardManager = new PortForwardManager();

        for (const ssh of sshManagers) {
          const host = ssh.getHost();

          const forwarder = portForwardManager.getForwarder(
            host,
            ssh,
            registryPort,
            registryPort,
          );

          await log.hostBlock(host, async () => {
            try {
              await forwarder.startForwarding();
              log.say("└── Port forwarding established", 2);
            } catch (error) {
              log.say(
                `Failed: ${
                  error instanceof Error ? error.message : String(error)
                }`,
                2,
              );
            }
          }, { indent: 1 });
        }
      }

      let allServices = config.getDeployableServices();

      if (deployOptions.services) {
        allServices = filterServicesByPatterns(
          allServices,
          deployOptions.services,
          config,
        );
      }

      // Use DeploymentOrchestrator for complex deployment workflow
      const orchestrator = new DeploymentOrchestrator(config, sshManagers);

      const orchestrationResult = await orchestrator.orchestrateDeployment(
        allServices,
        targetHosts,
        {
          version: globalOptions.version,
          allSshManagers: sshManagers,
        },
      );

      // Log structured deployment summary
      orchestrator.logDeploymentSummary(orchestrationResult);

      // Image cleanup for successful deployments
      if (
        allServices.length > 0 &&
        orchestrationResult.deploymentResults.some((r) => r.success)
      ) {
        log.section("Image Cleanup:");

        const pruneService = new ImagePruneService(
          config.builder.engine,
          config.project,
        );

        const maxRetain = Math.max(...allServices.map((s) => s.retain));
        log.say(
          `- Pruning old images (retaining last ${maxRetain} per service)`,
          1,
        );

        const pruneResults: Awaited<
          ReturnType<typeof pruneService.pruneImages>
        >[] = [];

        for (const ssh of sshManagers) {
          await log.hostBlock(ssh.getHost(), async () => {
            const result = await pruneService.pruneImages(ssh, {
              retain: maxRetain,
              removeDangling: true,
            });
            pruneResults.push(result);
          }, { indent: 1 });
        }

        const totalRemoved = pruneResults.reduce(
          (sum, r) => sum + r.imagesRemoved,
          0,
        );

        if (totalRemoved === 0) {
          log.say("- No old images were pruned", 1);
        }
      } else if (allServices.length === 0) {
        log.say("No services found to deploy");
      }

      log.say("\nDeployment completed successfully", 0);
    } catch (error) {
      await handleCommandError(error, {
        operation: "Deployment",
        component: "deploy",
        sshManagers: ctx?.sshManagers,
        projectName: ctx?.config?.project,
        targetHosts: ctx?.targetHosts,
      });
    } finally {
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

      if (ctx?.sshManagers) {
        cleanupSSHConnections(ctx.sshManagers);
      }
    }
  });
