import { log } from "../../utils/logger.ts";
import type { PushOptions, PushResult } from "../../types.ts";

/**
 * Service for pushing container images to registries
 * Handles platform-specific flags (e.g., --tls-verify for Podman)
 */
export class ImagePushService {
  constructor(private options: PushOptions) {}

  /**
   * Push a single image to the registry
   * @param imageName Full image name with tag
   * @param logCallback Optional callback for structured logging
   * @returns Push result
   */
  async pushImage(
    imageName: string,
    logCallback?: (message: string, type: "info" | "success" | "error") => void,
  ): Promise<PushResult> {
    try {
      await this.executePush(imageName, logCallback);
      return { imageName, success: true };
    } catch (error) {
      return {
        imageName,
        success: false,
        error: error instanceof Error ? error : new Error(String(error)),
      };
    }
  }

  /**
   * Push multiple images to the registry
   * @param imageNames Array of full image names with tags
   * @returns Array of push results
   */
  async pushImages(imageNames: string[]): Promise<PushResult[]> {
    const results: PushResult[] = [];

    for (const imageName of imageNames) {
      const result = await this.pushImage(imageName);
      results.push(result);
    }

    return results;
  }

  /**
   * Build push command arguments with platform-specific flags
   * @param imageName Image name to push
   * @returns Array of command arguments
   */
  private buildPushArgs(imageName: string): string[] {
    const args = ["push"];

    // Add --tls-verify=false for local registries when using podman
    if (
      this.options.registry.isLocal() && this.options.engine === "podman"
    ) {
      args.push("--tls-verify=false");
    }

    args.push(imageName);
    return args;
  }

  /**
   * Execute push command
   * @param imageName Image name to push
   * @param logCallback Optional callback for structured logging
   */
  private async executePush(
    imageName: string,
    logCallback?: (message: string, type: "info" | "success" | "error") => void,
  ): Promise<void> {
    if (logCallback) {
      logCallback(`Pushing ${imageName}`, "info");
    } else {
      log.info(`Pushing ${imageName}`, "registry");
    }

    const pushArgs = this.buildPushArgs(imageName);

    const pushCmd = new Deno.Command(this.options.engine, {
      args: pushArgs,
      stdout: this.options.globalOptions.verbose ? "inherit" : "piped",
      stderr: this.options.globalOptions.verbose ? "inherit" : "piped",
    });

    const pushResult = await pushCmd.output();

    if (pushResult.code !== 0) {
      const stderr = new TextDecoder().decode(pushResult.stderr);
      if (logCallback) {
        logCallback(`Failed to push ${imageName}`, "error");
        if (!this.options.globalOptions.verbose) {
          logCallback(stderr, "error");
        }
      } else {
        log.error(`Failed to push ${imageName}`, "registry");
        if (!this.options.globalOptions.verbose) {
          log.error(stderr, "registry");
        }
      }
      throw new Error(`Push failed for: ${imageName}`);
    }

    if (logCallback) {
      logCallback(`Pushed: ${imageName}`, "success");
    } else {
      log.success(`Pushed: ${imageName}`, "registry");
    }
  }
}
