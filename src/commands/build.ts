import { Command } from "@cliffy/command";
import { Configuration } from "../lib/configuration.ts";
import { GitUtils } from "../utils/git.ts";
import { RegistryManager } from "../utils/registry_manager.ts";
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

        // Determine container engine (use builder engine or fall back to project engine)
        const engine = config.builder.engine || config.engine;
        log.info(`Container engine: ${engine}`, "build");

        // Get services to build
        let servicesToBuild = config.getBuildServices();

        if (servicesToBuild.length === 0) {
          log.warn("No services with 'build' configuration found", "build");
          return;
        }

        // Filter by service pattern if specified
        if (buildOptions.services) {
          const servicePatterns = buildOptions.services.split(",").map((s) =>
            s.trim()
          );
          const matchingNames = config.getMatchingServiceNames(servicePatterns);
          servicesToBuild = servicesToBuild.filter((service) =>
            matchingNames.includes(service.name)
          );

          if (servicesToBuild.length === 0) {
            log.error(
              `No buildable services match pattern: ${buildOptions.services}`,
              "build",
            );
            Deno.exit(1);
          }
        }

        log.info(
          `Building ${servicesToBuild.length} service(s): ${
            servicesToBuild.map((s) => s.name).join(", ")
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
        const registry = config.builder.registry;
        const registryUrl = registry.getRegistryUrl();

        if (buildOptions.push && registry.isLocal()) {
          // Start local registry
          await log.group("Local Registry Setup", async () => {
            registryManager = new RegistryManager(engine, registry.port);

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
        for (const service of servicesToBuild) {
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
            const imageName = service.getImageName(registryUrl, versionTag);
            const latestImageName = service.getImageName(registryUrl, "latest");

            log.info(`Building image: ${imageName}`, "build");
            log.info(`Context: ${context}`, "build");
            log.info(`Dockerfile: ${dockerfile}`, "build");
            log.info(`Architecture(s): ${requiredArchs.join(", ")}`, "build");

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
              const platforms = requiredArchs.map((a) => `linux/${a}`).join(
                ",",
              );
              buildCmdArgs.push("--platform", platforms);
            } else if (requiredArchs.length === 1) {
              // Single architecture
              buildCmdArgs.push("--platform", `linux/${requiredArchs[0]}`);
            }

            // Add cache option
            if (buildOptions.noCache || !config!.builder.cache) {
              buildCmdArgs.push("--no-cache");
            }

            // Add context
            buildCmdArgs.push(context);

            // Execute build
            const buildCmd = new Deno.Command(engine, {
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
              throw new Error(`Build failed for service: ${service.name}`);
            }

            log.success(`Built image: ${imageName}`, "build");
            log.success(`Tagged as: ${latestImageName}`, "build");

            // Push to registry if requested
            if (buildOptions.push) {
              await log.group("Pushing to Registry", async () => {
                log.info(`Pushing ${imageName}`, "registry");

                // Push versioned image
                const pushCmd = new Deno.Command(engine, {
                  args: ["push", imageName],
                  stdout: globalOptions.verbose ? "inherit" : "piped",
                  stderr: globalOptions.verbose ? "inherit" : "piped",
                });

                const pushResult = await pushCmd.output();

                if (pushResult.code !== 0) {
                  const stderr = new TextDecoder().decode(pushResult.stderr);
                  log.error(`Failed to push ${imageName}`, "registry");
                  if (!globalOptions.verbose) {
                    log.error(stderr, "registry");
                  }
                  throw new Error(`Push failed for: ${imageName}`);
                }

                log.success(`Pushed: ${imageName}`, "registry");

                // Push latest tag
                log.info(`Pushing ${latestImageName}`, "registry");

                const pushLatestCmd = new Deno.Command(engine, {
                  args: ["push", latestImageName],
                  stdout: globalOptions.verbose ? "inherit" : "piped",
                  stderr: globalOptions.verbose ? "inherit" : "piped",
                });

                const pushLatestResult = await pushLatestCmd.output();

                if (pushLatestResult.code !== 0) {
                  const stderr = new TextDecoder().decode(
                    pushLatestResult.stderr,
                  );
                  log.error(`Failed to push ${latestImageName}`, "registry");
                  if (!globalOptions.verbose) {
                    log.error(stderr, "registry");
                  }
                  throw new Error(`Push failed for: ${latestImageName}`);
                }

                log.success(`Pushed: ${latestImageName}`, "registry");
              });
            }
          });
        }

        // Build summary
        log.success(
          `Successfully built ${servicesToBuild.length} service(s)`,
          "build",
        );
        log.info(`Version tag: ${versionTag}`, "build");
        if (buildOptions.push) {
          log.info(`Registry: ${registryUrl}`, "build");
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
