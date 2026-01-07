import type { ServiceConfiguration } from "../configuration/service.ts";
import { ImagePushService } from "./image_push_service.ts";
import { RegistryAuthService } from "./registry_auth_service.ts";
import { log } from "../../utils/logger.ts";
import type { BuildResult, BuildServiceOptions } from "../../types.ts";

/**
 * Service for building container images
 * Orchestrates the build process for one or more services
 */
export class BuildService {
  private imagePushService?: ImagePushService;
  private registryAuthService: RegistryAuthService;

  constructor(private options: BuildServiceOptions) {
    this.registryAuthService = new RegistryAuthService(
      options.engine,
      options.registry,
    );

    // Create ImagePushService if push is enabled
    if (options.push) {
      this.imagePushService = new ImagePushService({
        engine: options.engine,
        registry: options.registry,
        globalOptions: options.globalOptions,
      });
    }
  }

  /**
   * Authenticate to registry if needed (for remote registries)
   */
  private async ensureAuthenticated(): Promise<void> {
    // Use the new RegistryAuthService
    if (this.registryAuthService.requiresLocalAuth()) {
      await this.registryAuthService.authenticateLocally();
    }
  }

  /**
   * Build a single service
   * @param service Service configuration
   * @param versionTag Version tag for the image
   * @returns Build result
   */
  async buildService(
    service: ServiceConfiguration,
    versionTag: string,
  ): Promise<BuildResult> {
    try {
      const imageName = service.requiresBuild()
        ? this.options.registry.getFullImageName(
          service.project,
          service.name,
          versionTag,
        )
        : service.getImageName(undefined, versionTag);
      const latestImageName = service.requiresBuild()
        ? this.options.registry.getFullImageName(
          service.project,
          service.name,
          "latest",
        )
        : service.getImageName(undefined, "latest");

      log.say(`├── Building ${service.name} on ${this.options.engine}`, 2);
      log.say(`    Image: ${imageName}`, 3);
      log.say(`    Latest tag: ${latestImageName}`, 3);

      this.logBuildInfo(service, imageName);

      await this.executeBuild(service, imageName, latestImageName);

      if (this.options.push && this.imagePushService) {
        await this.ensureAuthenticated();

        log.say(`├── Pushing to registry`, 2);

        // Push versioned image
        log.say(`    Pushing: ${imageName}`, 3);
        const versionedResult = await this.imagePushService!.pushImage(
          imageName,
          (message, type) => {
            if (type === "info" || type === "success") {
              log.say(`    ${message}`, 3);
            } else {
              log.say(`    ${message}`, 3);
            }
          },
        );
        if (!versionedResult.success) {
          throw versionedResult.error ||
            new Error(`Push failed for: ${imageName}`);
        }

        // Push latest image
        log.say(`    Pushing: ${latestImageName}`, 3);
        const latestResult = await this.imagePushService!.pushImage(
          latestImageName,
          (message, type) => {
            if (type === "info" || type === "success") {
              log.say(`    ${message}`, 3);
            } else {
              log.say(`    ${message}`, 3);
            }
          },
        );
        if (!latestResult.success) {
          throw latestResult.error ||
            new Error(`Push failed for: ${latestImageName}`);
        }
        log.say(`└── ${service.name} built and pushed successfully`, 2);
      } else {
        log.say(`└── ${service.name} built successfully`, 2);
      }

      return {
        serviceName: service.name,
        success: true,
        imageName,
        latestImageName,
      };
    } catch (error) {
      log.say(
        `└── Build failed: ${
          error instanceof Error ? error.message : String(error)
        }`,
        2,
      );
      return {
        serviceName: service.name,
        success: false,
        imageName: "",
        latestImageName: "",
        error: error instanceof Error ? error.message : String(error),
      };
    }
  }

  /**
   * Build multiple services
   * @param services Array of service configurations
   * @param versionTag Version tag for the images
   * @returns Array of build results
   */
  async buildServices(
    services: ServiceConfiguration[],
    versionTag: string,
  ): Promise<BuildResult[]> {
    const results: BuildResult[] = [];

    for (const service of services) {
      await log.hostBlock(service.name, async () => {
        const result = await this.buildService(service, versionTag);
        results.push(result);

        if (!result.success) {
          throw result.error ||
            new Error(`Build failed for service: ${service.name}`);
        }
      }, { indent: 1 });
    }

    return results;
  }

  /**
   * Construct build command arguments
   * @param service Service configuration
   * @param imageName Image name with version tag
   * @param latestImageName Image name with latest tag
   * @returns Array of build command arguments
   */
  private buildCommandArgs(
    service: ServiceConfiguration,
    imageName: string,
    latestImageName: string,
  ): string[] {
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

    // Get architectures required by servers
    const requiredArchs = this.options.config
      .getRequiredArchitecturesForService(
        service.name,
      );

    // Add platform/architecture based on server requirements
    if (requiredArchs.length > 1) {
      // Multiple architectures - use platforms for multi-arch build
      const platforms = requiredArchs.map((a) => `linux/${a}`).join(",");
      buildCmdArgs.push("--platform", platforms);
    } else if (requiredArchs.length === 1) {
      // Single architecture
      buildCmdArgs.push("--platform", `linux/${requiredArchs[0]}`);
    }

    // Add cache option
    if (this.options.noCache || !this.options.cacheEnabled) {
      buildCmdArgs.push("--no-cache");
    }

    // Add context
    buildCmdArgs.push(context);

    return buildCmdArgs;
  }

  /**
   * Execute build command
   * @param service Service configuration
   * @param imageName Image name with version tag
   * @param latestImageName Image name with latest tag
   */
  private async executeBuild(
    service: ServiceConfiguration,
    imageName: string,
    latestImageName: string,
  ): Promise<void> {
    const buildCmdArgs = this.buildCommandArgs(
      service,
      imageName,
      latestImageName,
    );

    log.say(`├── Building image`, 2);
    log.say(
      `    Building with command: ${this.options.engine} ${
        buildCmdArgs.join(" ")
      }`,
      3,
    );

    const buildCmd = new Deno.Command(this.options.engine, {
      args: buildCmdArgs,
      stdout: this.options.globalOptions.verbose ? "inherit" : "piped",
      stderr: this.options.globalOptions.verbose ? "inherit" : "piped",
    });

    const buildResult = await buildCmd.output();

    if (buildResult.code !== 0) {
      const stderr = new TextDecoder().decode(buildResult.stderr);
      log.say(`├── ├── Build failed for ${service.name}`, 2);
      if (!this.options.globalOptions.verbose) {
        log.say(`├── ├── Error: ${stderr}`, 2);
      }
      throw new Error(`Build failed for service: ${service.name}`);
    }

    log.say(`├── ├── Build completed successfully`, 2);
  }

  /**
   * Log build information
   * @param service Service configuration
   * @param imageName Image name
   */
  private logBuildInfo(
    service: ServiceConfiguration,
    _imageName: string,
  ): void {
    const buildConfig = typeof service.build === "string"
      ? {
        context: service.build,
        dockerfile: "Dockerfile",
        args: {},
      }
      : service.build!;

    const context = buildConfig.context;
    const dockerfile = buildConfig.dockerfile || "Dockerfile";
    const requiredArchs = this.options.config
      .getRequiredArchitecturesForService(
        service.name,
      );
    const serversByArch = this.options.config
      .getServersByArchitectureForService(
        service.name,
      );

    log.say(`├── ├── Context: ${context}`, 2);
    log.say(`├── ├── Dockerfile: ${dockerfile}`, 2);
    log.say(`├── ├── Architecture(s): ${requiredArchs.join(", ")}`, 2);

    // Log server distribution by architecture
    for (const [arch, servers] of serversByArch.entries()) {
      log.say(`├── ├── ${arch}: ${servers.join(", ")}`, 2);
    }
  }
}
