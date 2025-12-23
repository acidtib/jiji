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
  log.section("Deployment Plan");
  log.say("Analyzing deployment configuration...");

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

  console.log();
  log.say("Deployment Plan");
  log.say("═".repeat(50));
  log.say(`Project: ${config.project}`);
  log.say(`Container Engine: ${config.builder.engine}`);
  log.say(`Registry: ${config.builder.registry.getRegistryUrl()}`);

  if (globalOptions.version) {
    log.say(`Version: ${globalOptions.version}`);
  }

  if (deployOptions.build && buildServices.length > 0) {
    log.say("Services to Build:");
    for (const service of buildServices) {
      log.say(`  • ${service.name}`);
      if (typeof service.build === "string") {
        log.say(`    Context: ${service.build}`, 2);
      } else if (service.build) {
        log.say(`    Context: ${service.build.context}`, 2);
        if (service.build.dockerfile) {
          log.say(`    Dockerfile: ${service.build.dockerfile}`, 2);
        }
        if (service.build.target) {
          log.say(`    Target: ${service.build.target}`, 2);
        }
      }
    }
  }

  log.say("Services to Deploy:");
  if (allServices.length === 0) {
    log.say("  No services to deploy");
  } else {
    for (const service of allServices) {
      log.say(`  ${service.name}`);

      if (service.image) {
        log.say(`    Image: ${service.image}`, 2);
      } else if (service.build) {
        log.say(
          `    Built from: ${
            typeof service.build === "string"
              ? service.build
              : service.build.context
          }`,
          2,
        );
      }

      if (service.servers.length > 0) {
        log.say(
          `    Servers: ${service.servers.map((s) => s.host).join(", ")}`,
          2,
        );
      }

      if (service.ports.length > 0) {
        log.say(`    Ports: ${service.ports.join(", ")}`, 2);
      }

      if (service.proxy?.enabled) {
        const hosts = service.proxy.hosts.length > 0
          ? service.proxy.hosts.join(", ")
          : "auto";
        log.say(`    Proxy: Enabled (${hosts})`, 2);
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
    log.say("\nOptions:");
    options.forEach((option) => log.say(`  • ${option}`));
  }

  log.say("═".repeat(50));
  console.log();

  if (deployOptions.yes) {
    log.say("Proceeding with deployment (--yes flag provided)");
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
      log.section("Service Deployment");
      log.say("Starting service deployment process");

      const config = await Configuration.load(
        globalOptions.environment,
        globalOptions.configFile,
      );
      log.say(`Container engine: ${config.builder.engine}`);

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
        const buildTracker = log.createStepTracker("Building Service Images");

        const servicesToBuild = config.getBuildServices();

        if (servicesToBuild.length === 0) {
          buildTracker.step("No services with 'build' configuration found");
        } else {
          let filteredServices = servicesToBuild;
          if (deployOptions.services) {
            filteredServices = filterServicesByPatterns(
              servicesToBuild,
              deployOptions.services,
              config,
            );
          }

          buildTracker.step(
            `Building ${filteredServices.length} service(s): ${
              filteredServices.map((s) => s.name).join(", ")
            }`,
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
            buildTracker.step("Setting up local registry...");
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

          buildTracker.step(`Version tag: ${versionTag}`);
          buildTracker.step(`Registry: ${registry.getRegistryUrl()}`);
        }

        buildTracker.finish();
      }

      ctx = await setupCommandContext(globalOptions);
      const { sshManagers, targetHosts } = ctx;

      if (config.builder.registry.isLocal()) {
        const portTracker = log.createStepTracker(
          "Setting Up Local Registry Access",
        );

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

          try {
            await forwarder.startForwarding();
            portTracker.remote(host, "Port forwarding established");
          } catch (error) {
            portTracker.remote(
              host,
              `Failed: ${
                error instanceof Error ? error.message : String(error)
              }`,
            );
          }
        }

        portTracker.finish();
      }

      let allServices = config.getDeployableServices();

      if (deployOptions.services) {
        allServices = filterServicesByPatterns(
          allServices,
          deployOptions.services,
          config,
        );
      }

      const serviceNames = allServices.map((s) => s.name).join(", ");
      log.say(
        `Deploying ${allServices.length} service${
          allServices.length === 1 ? "" : "s"
        }: ${serviceNames}`,
      );

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
        const pruneTracker = log.createStepTracker("Image Cleanup");

        const pruneService = new ImagePruneService(
          config.builder.engine,
          config.project,
        );

        const maxRetain = Math.max(...allServices.map((s) => s.retain));
        pruneTracker.step(
          `Pruning old images (retaining last ${maxRetain} per service)`,
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
          for (const result of pruneResults) {
            if (result.imagesRemoved > 0) {
              pruneTracker.remote(
                result.host,
                `Pruned ${result.imagesRemoved} image(s)`,
              );
            }
          }
        } else {
          pruneTracker.step("No old images to prune");
        }

        pruneTracker.finish();
      } else if (allServices.length === 0) {
        log.say("No services found to deploy");
      }

      console.log();
      log.say("Deployment process completed");
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
        const { cleanupSSHConnections } = await import(
          "../utils/command_helpers.ts"
        );
        cleanupSSHConnections(ctx.sshManagers);
      }
    }
  });
