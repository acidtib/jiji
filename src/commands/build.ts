import { Command } from "@cliffy/command";
import { Configuration } from "../lib/configuration.ts";
import { RegistryManager } from "../utils/registry_manager.ts";
import { VersionManager } from "../utils/version_manager.ts";
import { filterServicesByPatterns } from "../utils/config.ts";
import { BuildService } from "../lib/services/build_service.ts";
import { log } from "../utils/logger.ts";
import type { GlobalOptions } from "../types.ts";

interface BuildOptions extends GlobalOptions {
  noCache?: boolean;
  push?: boolean;
}

export const buildCommand = new Command()
  .description("Build container images for services")
  .option("--no-cache", "Build without using cache")
  .option("--push", "Push images to registry after building", { default: true })
  .action(async (options) => {
    let config: Configuration | undefined;
    let registryManager: RegistryManager | undefined;

    try {
      log.section("Service Build:");

      const buildOptions = options as unknown as BuildOptions;
      const globalOptions = options as unknown as GlobalOptions;

      config = await Configuration.load(
        globalOptions.environment,
        globalOptions.configFile,
      );
      const configPath = config.configPath || "unknown";
      log.say(`Configuration loaded from: ${configPath}`, 1);

      if (!config) throw new Error("Configuration failed to load");

      const engine = config.builder.engine;
      log.say(`Container engine: ${engine}`, 1);

      let servicesToBuild = config.getBuildServices();

      if (servicesToBuild.length === 0) {
        log.warn("No services with 'build' configuration found");
        return;
      }

      if (buildOptions.services) {
        servicesToBuild = filterServicesByPatterns(
          servicesToBuild,
          buildOptions.services,
          config,
        );
      }

      log.section("Build Plan:");
      log.say(`Project: ${config.project}`, 1);
      log.say(`Container Engine: ${config.builder.engine}`, 1);
      log.say(`Registry: ${config.builder.registry.getRegistryUrl()}`, 1);

      if (globalOptions.version) {
        log.say(`Version: ${globalOptions.version}`, 1);
      }

      console.log();
      log.say("Services to Build:", 1);
      for (const service of servicesToBuild) {
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

        if (service.hosts.length > 0) {
          log.say(
            `Hosts: ${service.hosts.join(", ")}`,
            3,
          );
        }
      }

      const optionsList: string[] = [];
      if (buildOptions.noCache) optionsList.push("No cache");
      if (!buildOptions.push) optionsList.push("Skip push");
      if (globalOptions.hosts) {
        optionsList.push(`Target hosts: ${globalOptions.hosts}`);
      }

      if (optionsList.length > 0) {
        console.log();
        log.say("Options:", 1);
        optionsList.forEach((option) => log.say(`${option}`, 2));
      }

      const versionTag = await VersionManager.determineVersionTag({
        customVersion: globalOptions.version,
        useGitSha: true,
        shortSha: true,
      });
      log.say(`\nVersion tag: ${versionTag}`, 1);

      const registry = config.builder.registry;

      if (buildOptions.push && registry.isLocal()) {
        log.section("Setting Up Local Registry:");

        registryManager = new RegistryManager(engine, registry.port);

        if (await registryManager.isRunning()) {
          log.say("- Local registry already running", 1);
          log.say("- Using existing registry", 1);
        } else {
          log.say("- Starting local registry", 1);
          await registryManager.start((message, type) => {
            if (type === "info") {
              log.say(`  ${message}`, 2);
            } else if (type === "success") {
              log.say(`  ${message}`, 2);
            } else {
              log.say(`  ${message}`, 2);
            }
          });
          log.say("- Local registry setup complete", 1);
        }
      }

      log.section("Building Service Images:");

      const buildService = new BuildService({
        engine,
        registry,
        config,
        globalOptions,
        noCache: buildOptions.noCache,
        push: buildOptions.push,
        cacheEnabled: config.builder.cache,
      });

      await buildService.buildServices(servicesToBuild, versionTag);

      if (buildOptions.push) {
        log.section("Pushing to Registry:");
        log.say(`- Registry: ${registry.getRegistryUrl()}`, 1);
      }

      log.success(
        `\nSuccessfully built ${servicesToBuild.length} service(s)`,
        0,
      );
    } catch (error) {
      console.log();
      log.error(
        `Build failed: ${
          error instanceof Error ? error.message : String(error)
        }`,
      );
      Deno.exit(1);
    }
  });
