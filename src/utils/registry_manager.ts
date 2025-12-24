import { log } from "./logger.ts";
import type { ContainerEngine } from "../lib/configuration/builder.ts";

/**
 * Registry status information
 */
export interface RegistryStatus {
  running: boolean;
  containerId?: string;
  port: number;
}

/**
 * Manager for local Docker/Podman registry
 * Handles starting, stopping, and checking status of the local registry container
 */
export class RegistryManager {
  private static readonly CONTAINER_NAME = "jiji-registry";
  private static readonly REGISTRY_IMAGE = "registry:2";
  private static readonly VOLUME_NAME = "jiji-registry-data";

  constructor(
    private engine: ContainerEngine,
    private port: number = 6767,
  ) {}

  /**
   * Check if the registry container is running
   */
  async isRunning(): Promise<boolean> {
    try {
      const command = new Deno.Command(this.engine, {
        args: [
          "ps",
          "--filter",
          `name=${RegistryManager.CONTAINER_NAME}`,
          "--format",
          "{{.Names}}",
        ],
        stdout: "piped",
        stderr: "piped",
      });

      const { code, stdout } = await command.output();
      if (code !== 0) {
        return false;
      }

      const output = new TextDecoder().decode(stdout).trim();
      return output === RegistryManager.CONTAINER_NAME;
    } catch {
      return false;
    }
  }

  /**
   * Start the local registry container
   */
  async start(logCallback?: (message: string, type: 'info' | 'success' | 'error') => void): Promise<void> {
    // Check if already running
    if (await this.isRunning()) {
      if (logCallback) {
        logCallback(`Local registry already running on port ${this.port}`, 'info');
      } else {
        log.info(
          `Local registry already running on port ${this.port}`,
          "registry",
        );
      }
      return;
    }

    // Check if container exists but is stopped
    const exists = await this.containerExists();
    if (exists) {
      if (logCallback) {
        logCallback("Starting existing registry container", 'info');
      } else {
        log.info("Starting existing registry container", "registry");
      }
      await this.startExistingContainer();
      return;
    }

    // Create and start new container
    if (logCallback) {
      logCallback(`Starting local registry on port ${this.port}`, 'info');
    } else {
      log.info(`Starting local registry on port ${this.port}`, "registry");
    }
    await this.createContainer();

    if (logCallback) {
      logCallback(`Local registry started on localhost:${this.port}`, 'success');
    } else {
      log.success(`Local registry started on localhost:${this.port}`, "registry");
    }
  }

  /**
   * Stop the local registry container
   */
  async stop(): Promise<void> {
    if (!await this.isRunning()) {
      log.info("Local registry is not running", "registry");
      return;
    }

    log.info("Stopping local registry", "registry");

    const command = new Deno.Command(this.engine, {
      args: ["stop", RegistryManager.CONTAINER_NAME],
      stdout: "piped",
      stderr: "piped",
    });

    const { code, stderr } = await command.output();
    if (code !== 0) {
      const error = new TextDecoder().decode(stderr);
      throw new Error(`Failed to stop registry: ${error}`);
    }

    log.success("Local registry stopped", "registry");
  }

  /**
   * Get the status of the local registry
   */
  async getStatus(): Promise<RegistryStatus> {
    const running = await this.isRunning();

    if (!running) {
      return { running: false, port: this.port };
    }

    // Get container ID
    const command = new Deno.Command(this.engine, {
      args: [
        "ps",
        "--filter",
        `name=${RegistryManager.CONTAINER_NAME}`,
        "--format",
        "{{.ID}}",
      ],
      stdout: "piped",
      stderr: "piped",
    });

    const { stdout } = await command.output();
    const containerId = new TextDecoder().decode(stdout).trim();

    return {
      running: true,
      containerId: containerId || undefined,
      port: this.port,
    };
  }

  /**
   * Ensure the registry volume exists
   */
  private async ensureVolume(): Promise<void> {
    const command = new Deno.Command(this.engine, {
      args: ["volume", "create", RegistryManager.VOLUME_NAME],
      stdout: "piped",
      stderr: "piped",
    });

    await command.output();
    // Ignore errors - volume may already exist
  }

  /**
   * Create a new registry container
   */
  private async createContainer(): Promise<void> {
    // Ensure volume exists
    await this.ensureVolume();

    const command = new Deno.Command(this.engine, {
      args: [
        "run",
        "-d",
        "--name",
        RegistryManager.CONTAINER_NAME,
        "--restart",
        "unless-stopped",
        "-p",
        `${this.port}:5000`,
        "-v",
        `${RegistryManager.VOLUME_NAME}:/var/lib/registry`,
        RegistryManager.REGISTRY_IMAGE,
      ],
      stdout: "piped",
      stderr: "piped",
    });

    const { code, stderr } = await command.output();
    if (code !== 0) {
      const error = new TextDecoder().decode(stderr);
      throw new Error(`Failed to create registry container: ${error}`);
    }
  }

  /**
   * Start an existing stopped container
   */
  private async startExistingContainer(): Promise<void> {
    const command = new Deno.Command(this.engine, {
      args: ["start", RegistryManager.CONTAINER_NAME],
      stdout: "piped",
      stderr: "piped",
    });

    const { code, stderr } = await command.output();
    if (code !== 0) {
      const error = new TextDecoder().decode(stderr);
      throw new Error(`Failed to start registry container: ${error}`);
    }

    log.success(`Local registry started on localhost:${this.port}`, "registry");
  }

  /**
   * Check if container exists (running or stopped)
   */
  private async containerExists(): Promise<boolean> {
    const command = new Deno.Command(this.engine, {
      args: [
        "ps",
        "-a",
        "--filter",
        `name=${RegistryManager.CONTAINER_NAME}`,
        "--format",
        "{{.Names}}",
      ],
      stdout: "piped",
      stderr: "piped",
    });

    const { code, stdout } = await command.output();
    if (code !== 0) {
      return false;
    }

    const output = new TextDecoder().decode(stdout).trim();
    return output === RegistryManager.CONTAINER_NAME;
  }

  /**
   * Remove the registry container (for cleanup)
   */
  async remove(): Promise<void> {
    // Stop first if running
    if (await this.isRunning()) {
      await this.stop();
    }

    if (!await this.containerExists()) {
      log.info("Registry container does not exist", "registry");
    } else {
      log.info("Removing local registry container", "registry");

      const command = new Deno.Command(this.engine, {
        args: ["rm", RegistryManager.CONTAINER_NAME],
        stdout: "piped",
        stderr: "piped",
      });

      const { code, stderr } = await command.output();
      if (code !== 0) {
        const error = new TextDecoder().decode(stderr);
        throw new Error(`Failed to remove registry container: ${error}`);
      }

      log.success("Local registry container removed", "registry");
    }

    // Clean up the volume
    await this.removeVolume();
  }

  /**
   * Remove the registry volume (for cleanup)
   */
  private async removeVolume(): Promise<void> {
    log.info("Removing local registry volume", "registry");

    const command = new Deno.Command(this.engine, {
      args: ["volume", "rm", RegistryManager.VOLUME_NAME],
      stdout: "piped",
      stderr: "piped",
    });

    const { code, stderr } = await command.output();
    if (code !== 0) {
      const error = new TextDecoder().decode(stderr);
      // Check if volume doesn't exist (this is not a critical error)
      if (error.toLowerCase().includes("no such volume")) {
        log.info("Registry volume does not exist", "registry");
        return;
      }
      throw new Error(`Failed to remove registry volume: ${error}`);
    }

    log.success("Local registry volume removed", "registry");
  }

  /**
   * Setup registry for build operations
   * Starts local registry if needed and returns registry URL
   * @returns Registry URL for image tagging
   */
  async setupForBuild(logCallback?: (message: string, type: 'info' | 'success' | 'error') => void): Promise<string> {
    if (!await this.isRunning()) {
      if (logCallback) {
        logCallback("Starting local registry", 'info');
      } else {
        log.info("Starting local registry", "registry");
      }
      await this.start(logCallback);
    } else {
      if (logCallback) {
        logCallback(
          `Local registry already running on port ${this.port}`,
          'info',
        );
      } else {
        log.info(
          `Local registry already running on port ${this.port}`,
          "registry",
        );
      }
    }

    return this.getRegistryUrl();
  }

  /**
   * Get registry URL for image naming
   * @returns Registry URL (e.g., "localhost:6767")
   */
  getRegistryUrl(): string {
    return `localhost:${this.port}`;
  }
}
