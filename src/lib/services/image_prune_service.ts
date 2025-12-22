/**
 * Service for pruning old Docker/Podman images while retaining recent deployments
 */

import type { ContainerEngine } from "../configuration/builder.ts";
import type { SSHManager } from "../../utils/ssh.ts";
import { log } from "../../utils/logger.ts";
import { executeBestEffort } from "../../utils/command_helpers.ts";
import type { PruneOptions, PruneResult } from "../../types.ts";

/**
 * Service for managing Docker/Podman image retention
 */
export class ImagePruneService {
  constructor(
    private engine: ContainerEngine,
    private project: string,
  ) {}

  /**
   * Prune old images on a specific host
   *
   * @param ssh SSH manager for the host
   * @param options Pruning options
   * @returns Pruning result
   */
  async pruneImages(
    ssh: SSHManager,
    options: PruneOptions = {},
  ): Promise<PruneResult> {
    const host = ssh.getHost();
    const retain = options.retain ?? 3;
    const removeDangling = options.removeDangling ?? true;

    try {
      log.status(
        `Pruning old images on ${host} (retaining last ${retain})`,
        "prune",
      );

      let imagesRemoved = 0;

      // Remove old tagged images for this project
      const taggedResult = await this.pruneTaggedImages(ssh, retain);
      imagesRemoved += taggedResult.count;

      // Remove dangling images if requested
      if (removeDangling) {
        const danglingResult = await this.pruneDanglingImages(ssh);
        imagesRemoved += danglingResult.count;
      }

      log.success(
        `Pruned ${imagesRemoved} image(s) on ${host}`,
        "prune",
      );

      return {
        host,
        success: true,
        imagesRemoved,
      };
    } catch (error) {
      const errorMessage = error instanceof Error
        ? error.message
        : String(error);
      log.error(`Failed to prune images on ${host}: ${errorMessage}`, "prune");

      return {
        host,
        success: false,
        imagesRemoved: 0,
        error: errorMessage,
      };
    }
  }

  /**
   * Prune tagged images, keeping the most recent N versions
   *
   * @param ssh SSH manager
   * @param retain Number of images to retain
   * @returns Count of images removed
   */
  private async pruneTaggedImages(
    ssh: SSHManager,
    retain: number,
  ): Promise<{ count: number }> {
    // Get all images for this project, sorted by creation time (newest first)
    const listImagesCmd =
      `${this.engine} images --format '{{.Repository}}:{{.Tag}}|{{.ID}}|{{.CreatedAt}}' --filter "reference=*/${this.project}-*" | sort -t'|' -k3 -r`;

    const listResult = await ssh.executeCommand(listImagesCmd);

    if (!listResult.success || !listResult.stdout.trim()) {
      log.debug("No project images found to prune", "prune");
      return { count: 0 };
    }

    // Parse image list
    const images = listResult.stdout.trim().split("\n").map((line) => {
      const [fullName, id, createdAt] = line.split("|");
      return { fullName, id, createdAt };
    });

    // Get list of images currently in use by containers
    const activeImagesCmd =
      `${this.engine} ps -a --format '{{.Image}}' --filter "label=project=${this.project}"`;
    const activeResult = await ssh.executeCommand(activeImagesCmd);
    const activeImages = new Set(
      activeResult.success ? activeResult.stdout.trim().split("\n") : [],
    );

    // Group images by service name (extract from repository name)
    const imagesByService = new Map<string, typeof images>();

    for (const image of images) {
      // Extract service name from image name pattern: registry/project-service:tag
      const match = image.fullName.match(/\/([\w-]+)-([^:]+):/);
      const serviceName = match ? match[2] : "unknown";

      if (!imagesByService.has(serviceName)) {
        imagesByService.set(serviceName, []);
      }
      imagesByService.get(serviceName)!.push(image);
    }

    // For each service, keep the most recent N images and remove the rest
    let removedCount = 0;
    const imagesToRemove: string[] = [];

    for (const [serviceName, serviceImages] of imagesByService) {
      log.debug(
        `Found ${serviceImages.length} image(s) for service ${serviceName}`,
        "prune",
      );

      // Skip the most recent N images, collect the rest for removal
      const oldImages = serviceImages.slice(retain);

      for (const image of oldImages) {
        // Don't remove images that are currently in use
        if (!activeImages.has(image.fullName)) {
          imagesToRemove.push(image.fullName);
          log.debug(`Marking ${image.fullName} for removal`, "prune");
        } else {
          log.debug(`Skipping active image ${image.fullName}`, "prune");
        }
      }
    }

    // Remove collected images
    if (imagesToRemove.length > 0) {
      for (const imageName of imagesToRemove) {
        await executeBestEffort(
          ssh,
          `${this.engine} rmi ${imageName}`,
          `removing image ${imageName}`,
        );
        removedCount++;
        log.debug(`Removed image ${imageName}`, "prune");
      }
    }

    return { count: removedCount };
  }

  /**
   * Remove dangling images (untagged images not used by any container)
   *
   * @param ssh SSH manager
   * @returns Count of images removed
   */
  private async pruneDanglingImages(
    ssh: SSHManager,
  ): Promise<{ count: number }> {
    // Remove dangling images with project label
    const pruneCmd =
      `${this.engine} image prune --force --filter "label=project=${this.project}"`;

    const result = await ssh.executeCommand(pruneCmd);

    if (!result.success) {
      log.debug(
        `Dangling image prune failed: ${result.stderr}`,
        "prune",
      );
      return { count: 0 };
    }

    // Try to extract count from output (format varies between Docker and Podman)
    const deletedMatch = result.stdout.match(/deleted:\s*(\d+)/i) ||
      result.stdout.match(/(\d+)\s*image.*deleted/i);

    const count = deletedMatch ? parseInt(deletedMatch[1], 10) : 0;

    if (count > 0) {
      log.debug(`Removed ${count} dangling image(s)`, "prune");
    }

    return { count };
  }

  /**
   * Prune images on multiple hosts
   *
   * @param sshManagers SSH managers for all hosts
   * @param options Pruning options
   * @returns Array of pruning results
   */
  async pruneImagesOnHosts(
    sshManagers: SSHManager[],
    options: PruneOptions = {},
  ): Promise<PruneResult[]> {
    const results: PruneResult[] = [];

    for (const ssh of sshManagers) {
      const result = await this.pruneImages(ssh, options);
      results.push(result);
    }

    return results;
  }
}
