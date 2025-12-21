import type { ContainerEngine } from "../configuration/builder.ts";
import type { RegistryConfiguration } from "../configuration/registry.ts";
import type { ServiceConfiguration } from "../configuration/service.ts";
import type { GlobalOptions } from "../../types.ts";
import { ImagePushService } from "./image_push_service.ts";
import { RegistryAuthService } from "./registry_auth_service.ts";
import { log } from "../../utils/logger.ts";

/**
 * Options for BuildService
 */
export interface BuildServiceOptions {
  engine: ContainerEngine;
  registry: RegistryConfiguration;
  globalOptions: GlobalOptions;
  noCache?: boolean;
  push?: boolean;
  cacheEnabled?: boolean;
}

/**
 * Result of building a service
 */
export interface BuildResult {
  serviceName: string;
  success: boolean;
  imageName: string;
  latestImageName: string;
  error?: Error;
}

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

      this.logBuildInfo(service, imageName);
      await this.executeBuild(service, imageName, latestImageName);

      log.success(`Built image: ${imageName}`, "build");
      log.success(`Tagged as: ${latestImageName}`, "build");

      if (this.options.push && this.imagePushService) {
        await this.ensureAuthenticated();

        await log.group("Pushing to Registry", async () => {
          const versionedResult = await this.imagePushService!.pushImage(
            imageName,
          );
          if (!versionedResult.success) {
            throw versionedResult.error ||
              new Error(`Push failed for: ${imageName}`);
          }

          const latestResult = await this.imagePushService!.pushImage(
            latestImageName,
          );
          if (!latestResult.success) {
            throw latestResult.error ||
              new Error(`Push failed for: ${latestImageName}`);
          }
        });
      }

      return {
        serviceName: service.name,
        success: true,
        imageName,
        latestImageName,
      };
    } catch (error) {
      return {
        serviceName: service.name,
        success: false,
        imageName: "",
        latestImageName: "",
        error: error instanceof Error ? error : new Error(String(error)),
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
      await log.group(`Building: ${service.name}`, async () => {
        const result = await this.buildService(service, versionTag);
        results.push(result);

        if (!result.success) {
          throw result.error ||
            new Error(`Build failed for service: ${service.name}`);
        }
      });
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
    const requiredArchs = service.getRequiredArchitectures();

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

    const buildCmd = new Deno.Command(this.options.engine, {
      args: buildCmdArgs,
      stdout: this.options.globalOptions.verbose ? "inherit" : "piped",
      stderr: this.options.globalOptions.verbose ? "inherit" : "piped",
    });

    const buildResult = await buildCmd.output();

    if (buildResult.code !== 0) {
      const stderr = new TextDecoder().decode(buildResult.stderr);
      log.error(`Failed to build ${service.name}`, "build");
      if (!this.options.globalOptions.verbose) {
        log.error(stderr, "build");
      }
      throw new Error(`Build failed for service: ${service.name}`);
    }
  }

  /**
   * Log build information
   * @param service Service configuration
   * @param imageName Image name
   */
  private logBuildInfo(
    service: ServiceConfiguration,
    imageName: string,
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
    const requiredArchs = service.getRequiredArchitectures();
    const serversByArch = service.getServersByArchitecture();

    log.info(`Building image: ${imageName}`, "build");
    log.info(`Context: ${context}`, "build");
    log.info(`Dockerfile: ${dockerfile}`, "build");
    log.info(`Architecture(s): ${requiredArchs.join(", ")}`, "build");

    // Log server distribution by architecture
    for (const [arch, servers] of serversByArch.entries()) {
      log.info(`${arch}: ${servers.join(", ")}`, "build");
    }
  }
}
