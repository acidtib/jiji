/**
 * Git file status information
 */
export interface GitFileStatus {
  status: string;
  file: string;
  statusDescription: string;
}

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
    const diffCommand = new Deno.Command("git", {
      args: ["diff", "--quiet"],
      stdout: "piped",
      stderr: "piped",
    });

    const diffResult = await diffCommand.output();
    const statusCommand = new Deno.Command("git", {
      args: ["status", "--porcelain"],
      stdout: "piped",
      stderr: "piped",
    });

    const statusResult = await statusCommand.output();

    const hasDiff = diffResult.code !== 0;
    const hasStatus = new TextDecoder().decode(statusResult.stdout).trim()
      .length > 0;

    return hasDiff || hasStatus;
  }

  /**
   * Get list of uncommitted files with their status
   * @returns Array of file status objects
   */
  static async getUncommittedFiles(): Promise<GitFileStatus[]> {
    const command = new Deno.Command("git", {
      args: ["status", "--porcelain"],
      stdout: "piped",
      stderr: "piped",
    });

    const { code, stdout, stderr } = await command.output();

    if (code !== 0) {
      const error = new TextDecoder().decode(stderr);
      throw new Error(`Failed to get git status: ${error}`);
    }

    const output = new TextDecoder().decode(stdout).trim();
    if (!output) {
      return [];
    }

    const files: GitFileStatus[] = [];
    const lines = output.split("\n");

    for (const line of lines) {
      if (!line.trim()) continue;

      const status = line.substring(0, 2);
      const file = line.substring(3).trim();

      const statusDescription = this.parseGitStatus(status);

      files.push({
        status,
        file,
        statusDescription,
      });
    }

    return files;
  }

  /**
   * Parse git status code into human-readable description
   */
  private static parseGitStatus(status: string): string {
    const index = status[0];
    const workingTree = status[1];

    const descriptions: string[] = [];

    switch (index) {
      case "M":
        descriptions.push("staged for commit");
        break;
      case "A":
        descriptions.push("added");
        break;
      case "D":
        descriptions.push("deleted");
        break;
      case "R":
        descriptions.push("renamed");
        break;
      case "C":
        descriptions.push("copied");
        break;
      case "U":
        descriptions.push("unmerged");
        break;
    }

    switch (workingTree) {
      case "M":
        descriptions.push("modified");
        break;
      case "D":
        descriptions.push("deleted");
        break;
      case "?":
        descriptions.push("untracked");
        break;
    }

    if (descriptions.length === 0) {
      return "changed";
    }

    return descriptions.join(", ");
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
