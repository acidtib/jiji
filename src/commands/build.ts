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
      const tracker = log.createStepTracker("Service Build");

      const buildOptions = options as unknown as BuildOptions;
      const globalOptions = options as unknown as GlobalOptions;

      tracker.step("Loading configuration");
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

      log.section("Image Building");
      log.say(
        `Building ${servicesToBuild.length} service(s): ${
          servicesToBuild.map((s) => s.name).join(", ")
        }`,
      );

      const versionTag = await VersionManager.determineVersionTag({
        customVersion: globalOptions.version,
        useGitSha: true,
        shortSha: true,
      });
      log.say(`Version tag: ${versionTag}`, 1);

      const registry = config.builder.registry;

      if (buildOptions.push && registry.isLocal()) {
        tracker.step("Setting up local registry");
        registryManager = new RegistryManager(engine, registry.port);
        await registryManager.setupForBuild();
      }

      const buildService = new BuildService({
        engine,
        registry,
        globalOptions,
        noCache: buildOptions.noCache,
        push: buildOptions.push,
        cacheEnabled: config.builder.cache,
      });

      tracker.step(`Building ${servicesToBuild.length} service(s)`);
      await buildService.buildServices(servicesToBuild, versionTag);

      if (buildOptions.push) {
        log.say(`Registry: ${registry.getRegistryUrl()}`, 1);
      }

      tracker.finish();

      console.log();
      log.success(
        `Successfully built ${servicesToBuild.length} service(s)`,
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
