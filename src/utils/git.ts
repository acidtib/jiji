/**
 * Git utility functions for version management
 */
export class GitUtils {
  /**
   * Get the current commit SHA
   * @param short If true, returns short SHA (7 chars), otherwise full SHA
   */
  static async getCommitSHA(short = true): Promise<string> {
    const args = short
      ? ["rev-parse", "--short", "HEAD"]
      : ["rev-parse", "HEAD"];

    const command = new Deno.Command("git", {
      args,
      stdout: "piped",
      stderr: "piped",
    });

    const { code, stdout, stderr } = await command.output();

    if (code !== 0) {
      const error = new TextDecoder().decode(stderr);
      throw new Error(`Failed to get git commit SHA: ${error}`);
    }

    return new TextDecoder().decode(stdout).trim();
  }

  /**
   * Get the current branch name
   */
  static async getCurrentBranch(): Promise<string> {
    const command = new Deno.Command("git", {
      args: ["rev-parse", "--abbrev-ref", "HEAD"],
      stdout: "piped",
      stderr: "piped",
    });

    const { code, stdout, stderr } = await command.output();

    if (code !== 0) {
      const error = new TextDecoder().decode(stderr);
      throw new Error(`Failed to get current branch: ${error}`);
    }

    return new TextDecoder().decode(stdout).trim();
  }

  /**
   * Check if there are uncommitted changes in the repository
   */
  static async hasUncommittedChanges(): Promise<boolean> {
    // Check for staged and unstaged changes
    const diffCommand = new Deno.Command("git", {
      args: ["diff", "--quiet"],
      stdout: "piped",
      stderr: "piped",
    });

    const diffResult = await diffCommand.output();

    // Check for untracked files
    const statusCommand = new Deno.Command("git", {
      args: ["status", "--porcelain"],
      stdout: "piped",
      stderr: "piped",
    });

    const statusResult = await statusCommand.output();

    // diff --quiet returns non-zero if there are changes
    const hasDiff = diffResult.code !== 0;

    // status --porcelain returns output if there are untracked/changed files
    const hasStatus = new TextDecoder().decode(statusResult.stdout).trim()
      .length > 0;

    return hasDiff || hasStatus;
  }

  /**
   * Get the root directory of the git repository
   */
  static async getRepoRoot(): Promise<string> {
    const command = new Deno.Command("git", {
      args: ["rev-parse", "--show-toplevel"],
      stdout: "piped",
      stderr: "piped",
    });

    const { code, stdout, stderr } = await command.output();

    if (code !== 0) {
      const error = new TextDecoder().decode(stderr);
      throw new Error(`Failed to get repository root: ${error}`);
    }

    return new TextDecoder().decode(stdout).trim();
  }

  /**
   * Check if the current directory is inside a git repository
   */
  static async isGitRepository(): Promise<boolean> {
    try {
      const command = new Deno.Command("git", {
        args: ["rev-parse", "--git-dir"],
        stdout: "piped",
        stderr: "piped",
      });

      const { code } = await command.output();
      return code === 0;
    } catch {
      return false;
    }
  }

  /**
   * Check if git is installed and available
   */
  static async isGitInstalled(): Promise<boolean> {
    try {
      const command = new Deno.Command("git", {
        args: ["--version"],
        stdout: "piped",
        stderr: "piped",
      });

      const { code } = await command.output();
      return code === 0;
    } catch {
      return false;
    }
  }
}
