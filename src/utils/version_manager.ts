import { GitUtils } from "./git.ts";
import { log } from "./logger.ts";
import { ulid } from "@std/ulid";

/**
 * Options for version tag determination
 */
export interface VersionOptions {
  customVersion?: string;
  useGitSha?: boolean;
  shortSha?: boolean;
  requireVersion?: boolean; // If true, throw error when no version can be determined
  serviceName?: string; // Service name for better error messages
  isImageService?: boolean; // Whether this is an image service (not built from source)
}

/**
 * Manages version tag determination for container images
 * Supports custom versions, git SHA-based versioning, or ULID generation
 */
export class VersionManager {
  /**
   * Determine version tag from options or git repository
   *
   * Strategy:
   * 1. If --version is provided, use that
   * 2. For image-based services (not built), use image tag if present, otherwise "latest"
   * 3. For build services in git repo, use git SHA
   * 4. For build services not in git repo, require --version or generate ULID
   *
   * @param options Version options
   * @returns Version tag string
   */
  static async determineVersionTag(
    options: VersionOptions = {},
  ): Promise<string> {
    const {
      customVersion,
      useGitSha = true,
      shortSha = true,
      requireVersion = false,
      serviceName = "service",
      isImageService = false,
    } = options;

    // Custom version takes precedence (from --version flag)
    if (customVersion) {
      log.info(`Using custom version tag: ${customVersion}`, "version");
      return customVersion;
    }

    // For image-based services, default to "latest"
    if (isImageService) {
      log.info(
        `Using 'latest' tag for image-based service ${serviceName}`,
        "version",
      );
      return "latest";
    }

    // For build services, try to use git SHA if in a git repo
    if (useGitSha) {
      const isGitRepo = await GitUtils.isGitRepository();

      if (isGitRepo) {
        // Get git SHA
        const versionTag = await GitUtils.getCommitSHA(shortSha);
        log.info(`Using git SHA as version: ${versionTag}`, "version");

        // Warn about uncommitted changes
        await this.checkUncommittedChanges();

        return versionTag;
      } else {
        // Not in a git repository
        if (requireVersion) {
          throw new Error(
            `Service '${serviceName}' requires a version tag. Not in a git repository. Use --version to specify a tag.`,
          );
        }

        // Generate ULID as fallback for build services not in git repo
        const ulidVersion = ulid();
        log.warn(
          `Not in a git repository. Generated ULID version: ${ulidVersion}`,
          "version",
        );
        return ulidVersion;
      }
    }

    // Final fallback to 'latest' (should rarely reach here)
    log.warn("No version specified, using 'latest'", "version");
    return "latest";
  }

  /**
   * Check for uncommitted changes and warn user with file details
   */
  static async checkUncommittedChanges(): Promise<void> {
    if (await GitUtils.hasUncommittedChanges()) {
      log.warn(
        "You have uncommitted changes. The build will be tagged with the current commit SHA.",
        "version",
      );
      log.warn(
        "The following files are NOT included in this deployment:",
        "version",
      );

      // Get and display uncommitted files
      const uncommittedFiles = await GitUtils.getUncommittedFiles();

      // Group files by status for better readability
      const maxFilesToShow = 20;
      const filesToDisplay = uncommittedFiles.slice(0, maxFilesToShow);

      for (const fileStatus of filesToDisplay) {
        log.warn(
          `  ${fileStatus.status} ${fileStatus.file} (${fileStatus.statusDescription})`,
          "version",
        );
      }

      if (uncommittedFiles.length > maxFilesToShow) {
        log.warn(
          `  ... and ${uncommittedFiles.length - maxFilesToShow} more file(s)`,
          "version",
        );
      }

      log.warn(
        `Total uncommitted files: ${uncommittedFiles.length}`,
        "version",
      );
    }
  }
}
