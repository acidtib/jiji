import { join, resolve } from "@std/path";
import { exists } from "@std/fs";
import type { SSHManager } from "./ssh.ts";
import { log } from "./logger.ts";

/**
 * Error pages glob pattern matching 4xx.html and 5xx.html files
 */

/**
 * Error pages manager for uploading and managing custom error pages
 */
export class ErrorPagesManager {
  private readonly errorPagesHostDir: string;
  private readonly errorPagesContainerDir: string;

  constructor(
    private engine: "docker" | "podman",
    private ssh: SSHManager,
    private project: string,
    private service: string,
    private proxyContainerName = "kamal-proxy",
  ) {
    // Store error pages in .jiji/{project}/proxy/{service}/error_pages/
    this.errorPagesHostDir = `.jiji/${project}/proxy/${service}/error_pages`;
    this.errorPagesContainerDir =
      `/home/kamal-proxy/${project}/${service}/error_pages`;
  }

  /**
   * Checks if error pages directory exists locally
   */
  async hasErrorPages(errorPagesPath: string): Promise<boolean> {
    try {
      const absolutePath = resolve(errorPagesPath);
      return await exists(absolutePath, { isDirectory: true });
    } catch {
      return false;
    }
  }

  /**
   * Gets list of error page files in the directory
   */
  async getErrorPageFiles(errorPagesPath: string): Promise<string[]> {
    const absolutePath = resolve(errorPagesPath);
    const files: string[] = [];

    try {
      for await (const entry of Deno.readDir(absolutePath)) {
        if (entry.isFile && this.isErrorPageFile(entry.name)) {
          files.push(join(absolutePath, entry.name));
        }
      }
    } catch (error) {
      log.error(
        `Failed to read error pages directory: ${
          error instanceof Error ? error.message : String(error)
        }`,
        "error-pages",
      );
    }

    return files;
  }

  /**
   * Checks if a filename matches the error pages pattern (4xx.html or 5xx.html)
   */
  private isErrorPageFile(filename: string): boolean {
    const pattern = /^[45]\d{2}\.html$/;
    return pattern.test(filename);
  }

  /**
   * Creates the error pages directory on the remote server
   */
  async createRemoteDirectory(version: string): Promise<string> {
    const remotePath = join(this.errorPagesHostDir, version);
    const command = `mkdir -p ${remotePath}`;

    const result = await this.ssh.executeCommand(command);
    if (!result.success) {
      throw new Error(
        `Failed to create remote error pages directory: ${
          result.stderr || result.stdout
        }`,
      );
    }

    return remotePath;
  }

  /**
   * Uploads error pages to the remote server
   */
  async uploadErrorPages(
    errorPagesPath: string,
    version: string,
  ): Promise<string | null> {
    // Check if directory exists
    if (!await this.hasErrorPages(errorPagesPath)) {
      log.warn(
        `Error pages directory not found: ${errorPagesPath}`,
        "error-pages",
      );
      return null;
    }

    // Get error page files
    const files = await this.getErrorPageFiles(errorPagesPath);
    if (files.length === 0) {
      log.warn(
        `No error page files (4xx.html, 5xx.html) found in: ${errorPagesPath}`,
        "error-pages",
      );
      return null;
    }

    log.info(
      `Found ${files.length} error page file(s): ${
        files.map((f) => f.split("/").pop()).join(", ")
      }`,
      "error-pages",
    );

    // Create remote directory
    const remotePath = await this.createRemoteDirectory(version);

    // Upload each file
    for (const localFile of files) {
      const filename = localFile.split("/").pop()!;
      const remoteFile = join(remotePath, filename);

      try {
        // Read local file content
        const content = await Deno.readTextFile(localFile);

        // Upload via SSH (create file with content)
        const uploadCommand = `cat > ${remoteFile} << 'EOF'\n${content}\nEOF`;
        const result = await this.ssh.executeCommand(uploadCommand);

        if (!result.success) {
          log.error(
            `Failed to upload ${filename}: ${result.stderr || result.stdout}`,
            "error-pages",
          );
          continue;
        }

        log.success(`Uploaded ${filename} to ${remoteFile}`, "error-pages");
      } catch (error) {
        log.error(
          `Error uploading ${filename}: ${
            error instanceof Error ? error.message : String(error)
          }`,
          "error-pages",
        );
      }
    }

    // Copy files into the proxy container
    await this.copyToProxyContainer(remotePath, version);

    return join(this.errorPagesContainerDir, version);
  }

  /**
   * Copies error pages from host to proxy container
   */
  private async copyToProxyContainer(
    hostPath: string,
    version: string,
  ): Promise<void> {
    const containerPath = join(this.errorPagesContainerDir, version);

    // Create directory in container
    const mkdirCommand =
      `${this.engine} exec ${this.proxyContainerName} mkdir -p ${containerPath}`;
    await this.ssh.executeCommand(mkdirCommand);

    // Copy files from host to container
    const copyCommand =
      `${this.engine} cp ${hostPath}/. ${this.proxyContainerName}:${containerPath}/`;
    const result = await this.ssh.executeCommand(copyCommand);

    if (!result.success) {
      throw new Error(
        `Failed to copy error pages to container: ${
          result.stderr || result.stdout
        }`,
      );
    }

    log.success(
      `Error pages copied to proxy container: ${containerPath}`,
      "error-pages",
    );
  }

  /**
   * Cleans up old error pages directories
   */
  async cleanupOldVersions(
    _currentVersion: string,
    keepVersions = 3,
  ): Promise<void> {
    try {
      // List all version directories
      const listCommand =
        `find ${this.errorPagesHostDir} -maxdepth 1 -type d -name "*" | sort -r`;
      const result = await this.ssh.executeCommand(listCommand);

      if (!result.success) return;

      const directories = result.stdout
        .split("\n")
        .map((d) => d.trim())
        .filter((d) => d && d !== this.errorPagesHostDir);

      // Keep only the most recent versions
      const toDelete = directories.slice(keepVersions);

      for (const dir of toDelete) {
        const deleteCommand = `rm -rf ${dir}`;
        await this.ssh.executeCommand(deleteCommand);
        log.info(`Cleaned up old error pages: ${dir}`, "error-pages");
      }
    } catch (error) {
      log.error(
        `Failed to cleanup old error pages: ${
          error instanceof Error ? error.message : String(error)
        }`,
        "error-pages",
      );
    }
  }
}
