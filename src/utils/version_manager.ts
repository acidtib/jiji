import { GitUtils } from "./git.ts";
import { log } from "./logger.ts";

/**
 * Options for version tag determination
 */
export interface VersionOptions {
  customVersion?: string;
  useGitSha?: boolean;
  shortSha?: boolean;
}

/**
 * Manages version tag determination for container images
 * Supports custom versions or automatic git SHA-based versioning
 */
export class VersionManager {
  /**
   * Determine version tag from options or git repository
   * @param options Version options
   * @returns Version tag string
   */
  static async determineVersionTag(
    options: VersionOptions = {},
  ): Promise<string> {
    const { customVersion, useGitSha = true, shortSha = true } = options;

    // Custom version takes precedence
    if (customVersion) {
      log.info(`Using custom version tag: ${customVersion}`, "version");
      return customVersion;
    }

    // Check if we should use git SHA
    if (useGitSha) {
      // Validate git repository exists
      await this.validateGitRepository();

      // Get git SHA
      const versionTag = await GitUtils.getCommitSHA(shortSha);
      log.info(`Using git SHA as version: ${versionTag}`, "version");

      // Warn about uncommitted changes
      await this.checkUncommittedChanges();

      return versionTag;
    }

    // Fallback to 'latest' if no options provided
    log.warn("No version specified, using 'latest'", "version");
    return "latest";
  }

  /**
   * Validate that we're in a git repository
   * @throws Error if not in a git repository
   */
  static async validateGitRepository(): Promise<void> {
    if (!await GitUtils.isGitRepository()) {
      throw new Error(
        "Not in a git repository. Either initialize git or use --version to specify a tag.",
      );
    }
  }

  /**
   * Check for uncommitted changes and warn user
   */
  static async checkUncommittedChanges(): Promise<void> {
    if (await GitUtils.hasUncommittedChanges()) {
      log.warn(
        "You have uncommitted changes. The build will be tagged with the current commit SHA.",
        "version",
      );
    }
  }
}
