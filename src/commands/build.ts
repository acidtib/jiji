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
      await log.group("Service Build", async () => {
        log.info("Starting service build process", "build");

        // Cast options to BuildOptions
        const buildOptions = options as unknown as BuildOptions;
        const globalOptions = options as unknown as GlobalOptions;

        // Load configuration
        config = await Configuration.load(
          globalOptions.environment,
          globalOptions.configFile,
        );
        const configPath = config.configPath || "unknown";
        log.success(`Configuration loaded from: ${configPath}`, "config");

        if (!config) throw new Error("Configuration failed to load");

        // Get container engine from builder configuration
        const engine = config.builder.engine;
        log.info(`Container engine: ${engine}`, "build");

        // Get services to build
        let servicesToBuild = config.getBuildServices();

        if (servicesToBuild.length === 0) {
          log.warn("No services with 'build' configuration found", "build");
          return;
        }

        // Filter by service pattern if specified
        if (buildOptions.services) {
          servicesToBuild = filterServicesByPatterns(
            servicesToBuild,
            buildOptions.services,
            config,
          );
        }

        log.info(
          `Building ${servicesToBuild.length} service(s): ${
            servicesToBuild.map((s) => s.name).join(", ")
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
        const registry = config.builder.registry;

        if (buildOptions.push && registry.isLocal()) {
          // Start local registry
          await log.group("Local Registry Setup", async () => {
            registryManager = new RegistryManager(engine, registry.port);
            await registryManager.setupForBuild();
          });
        }

        // Create BuildService and build all services
        const buildService = new BuildService({
          engine,
          registry,
          globalOptions,
          noCache: buildOptions.noCache,
          push: buildOptions.push,
          cacheEnabled: config.builder.cache,
        });

        await buildService.buildServices(servicesToBuild, versionTag);

        // Build summary
        log.success(
          `Successfully built ${servicesToBuild.length} service(s)`,
          "build",
        );
        log.info(`Version tag: ${versionTag}`, "build");
        if (buildOptions.push) {
          log.info(`Registry: ${registry.getRegistryUrl()}`, "build");
        }
      });
    } catch (error) {
      log.error(
        `Build failed: ${
          error instanceof Error ? error.message : String(error)
        }`,
        "build",
      );
      Deno.exit(1);
    }
  });
