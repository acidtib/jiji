import type { ProxyConfiguration } from "../lib/configuration/proxy.ts";
import type { SSHManager } from "./ssh.ts";

/**
 * ProxyCommands generates container engine commands for managing kamal-proxy
 *
 * This class provides methods to generate and execute shell commands over SSH for:
 * - Installing/booting kamal-proxy
 * - Starting/stopping the proxy container
 * - Deploying service targets
 * - Removing proxy containers
 */
export class ProxyCommands {
  private containerName = "kamal-proxy";
  private networkName = "jiji";
  private configVolume = "kamal-proxy-config";

  // For rootless containers, use high ports internally to avoid permission issues
  private readonly internalHttpPort = 8080;
  private readonly internalHttpsPort = 8443;

  constructor(
    private engine: "docker" | "podman",
    private ssh: SSHManager,
  ) {}

  /**
   * Ensure the jiji network exists
   */
  async ensureNetwork(): Promise<void> {
    const command =
      `${this.engine} network create ${this.networkName} 2>/dev/null || true`;
    await this.ssh.executeCommand(command);
  }

  /**
   * Check if kamal-proxy is running
   */
  async isRunning(): Promise<boolean> {
    const command =
      `${this.engine} ps --filter "name=^${this.containerName}$" --format "{{.Names}}" | grep -q "${this.containerName}"`;
    const result = await this.ssh.executeCommand(command);
    return result.success;
  }

  /**
   * Get kamal-proxy version
   */
  async getVersion(): Promise<string | null> {
    const command =
      `${this.engine} inspect ${this.containerName} --format '{{.Config.Image}}' 2>/dev/null | awk -F: '{print $NF}'`;
    const result = await this.ssh.executeCommand(command);
    return result.success ? result.stdout.trim() : null;
  }

  /**
   * Pull the kamal-proxy image
   */
  async pullImage(): Promise<void> {
    const image = "docker.io/basecamp/kamal-proxy:latest";
    const command = `${this.engine} pull ${image}`;
    const result = await this.ssh.executeCommand(command);

    if (!result.success) {
      throw new Error(
        `Failed to pull kamal-proxy image: ${result.stderr || result.stdout}`,
      );
    }
  }

  /**
   * Run kamal-proxy container
   */
  async run(options?: {
    publish?: boolean;
    httpPort?: number;
    httpsPort?: number;
  }): Promise<void> {
    const {
      publish = true,
      httpPort = 80,
      httpsPort = 443,
    } = options || {};

    // Pull the image first
    await this.pullImage();

    // Remove existing container if it exists
    await this.removeContainer();

    const publishArgs = publish
      ? `-p ${httpPort}:${this.internalHttpPort} -p ${httpsPort}:${this.internalHttpsPort}`
      : "";

    const command =
      `${this.engine} run --name ${this.containerName} --network ${this.networkName} --detach --restart unless-stopped --volume ${this.configVolume}:/home/kamal-proxy/.config/kamal-proxy ${publishArgs} docker.io/basecamp/kamal-proxy:latest kamal-proxy run --http-port ${this.internalHttpPort} --https-port ${this.internalHttpsPort}`;

    const result = await this.ssh.executeCommand(command);
    if (!result.success) {
      throw new Error(
        `Failed to run kamal-proxy: ${result.stderr || result.stdout}`,
      );
    }

    // Give the container a moment to start before we begin polling
    await new Promise((resolve) => setTimeout(resolve, 500));
  }

  /**
   * Start existing proxy container
   */
  async start(): Promise<void> {
    const command = `${this.engine} start ${this.containerName}`;
    const result = await this.ssh.executeCommand(command);
    if (!result.success) {
      throw new Error(
        `Failed to start kamal-proxy: ${result.stderr || result.stdout}`,
      );
    }
  }

  /**
   * Stop proxy container
   */
  async stop(): Promise<void> {
    const command = `${this.engine} stop ${this.containerName}`;
    await this.ssh.executeCommand(command);
  }

  /**
   * Start or run the proxy (idempotent boot)
   */
  async boot(options?: {
    publish?: boolean;
    httpPort?: number;
    httpsPort?: number;
  }): Promise<void> {
    // Try to start first, if that fails, run a new container
    try {
      await this.start();
    } catch {
      await this.run(options);
    }

    // Always wait for the container to be ready after boot
    await this.waitForReady();
  }

  /**
   * Wait for the proxy container to be ready
   */
  private async waitForReady(maxAttempts = 30, delayMs = 1000): Promise<void> {
    for (let i = 0; i < maxAttempts; i++) {
      const command =
        `${this.engine} inspect ${this.containerName} --format '{{.State.Status}}'`;
      const result = await this.ssh.executeCommand(command);

      if (result.success && result.stdout.trim() === "running") {
        // Wait an additional moment for the process inside to be ready
        await new Promise((resolve) => setTimeout(resolve, 2000));
        return;
      }

      // Container might be in a different state (created, restarting, etc.)
      if (i % 5 === 0 && result.success) {
        // Log status every 5 attempts for debugging
        console.log(
          `Waiting for kamal-proxy... (status: ${result.stdout.trim()})`,
        );
      }

      await new Promise((resolve) => setTimeout(resolve, delayMs));
    }

    // Get final state and logs for debugging
    const stateCmd =
      `${this.engine} inspect ${this.containerName} --format '{{.State.Status}}: {{.State.Error}}'`;
    const stateResult = await this.ssh.executeCommand(stateCmd);
    const logsCmd = `${this.engine} logs --tail 20 ${this.containerName} 2>&1`;
    const logsResult = await this.ssh.executeCommand(logsCmd);

    throw new Error(
      `kamal-proxy container did not become ready after ${
        maxAttempts * delayMs
      }ms. State: ${stateResult.stdout.trim()}. Logs: ${logsResult.stdout}`,
    );
  }

  /**
   * Remove proxy container
   */
  async removeContainer(): Promise<void> {
    const command =
      `${this.engine} container rm -f ${this.containerName} 2>/dev/null || true`;
    await this.ssh.executeCommand(command);
  }

  /**
   * Remove proxy image
   */
  async removeImage(): Promise<void> {
    const command =
      `${this.engine} image rm -f basecamp/kamal-proxy 2>/dev/null || true`;
    await this.ssh.executeCommand(command);
  }

  /**
   * Deploy a service target to the proxy
   * @param service - Service name
   * @param containerName - Name of the container to route to
   * @param config - Proxy configuration for the service
   * @param appPort - Port the app container is listening on
   */
  async deploy(
    service: string,
    containerName: string,
    config: ProxyConfiguration,
    appPort: number,
  ): Promise<void> {
    const options: string[] = [];

    // Target is the container:port to route to
    options.push(`--target=${containerName}:${appPort}`);

    // Host/domain configuration
    if (config.host) {
      options.push(`--host=${config.host}`);
    }

    // SSL/TLS
    if (config.ssl) {
      options.push("--tls");
    }

    // Health check configuration
    const healthcheck = config.healthcheck;
    if (healthcheck?.path) {
      options.push(`--health-check-path=${healthcheck.path}`);
    }
    if (healthcheck?.interval) {
      options.push(`--health-check-interval=${healthcheck.interval}`);
    }

    const optionsStr = options.join(" ");
    const command =
      `${this.engine} exec ${this.containerName} kamal-proxy deploy ${service} ${optionsStr}`;

    const result = await this.ssh.executeCommand(command);
    if (!result.success) {
      throw new Error(
        `Failed to deploy service ${service} to proxy: ${
          result.stderr || result.stdout
        }`,
      );
    }
  }

  /**
   * Remove a service from the proxy
   */
  async remove(service: string): Promise<void> {
    const command =
      `${this.engine} exec ${this.containerName} kamal-proxy remove ${service} 2>/dev/null || true`;
    await this.ssh.executeCommand(command);
  }

  /**
   * List all services in the proxy
   */
  async list(): Promise<string[]> {
    const command =
      `${this.engine} exec ${this.containerName} kamal-proxy list`;
    const result = await this.ssh.executeCommand(command);

    if (!result.success) {
      return [];
    }

    return result.stdout
      .split("\n")
      .map((line) => line.trim())
      .filter((line) => line.length > 0);
  }

  /**
   * Get proxy container info
   */
  async info(): Promise<string> {
    const command = `${this.engine} ps --filter "name=^${this.containerName}$"`;
    const result = await this.ssh.executeCommand(command);
    return result.stdout;
  }
}

/**
 * Extract the app port from a service's port mappings
 * Looks for the first port mapping and extracts the container port
 * @param ports - Array of port mappings like ["3000:80", "443:443"]
 * @returns The container port number
 */
export function extractAppPort(ports: string[]): number {
  if (ports.length === 0) {
    return 3000; // Default app port
  }

  // Get first port mapping
  const firstPort = ports[0];

  // Handle format: [host_ip:]host_port:container_port[/protocol]
  // Extract container_port
  const parts = firstPort.split(":");
  let containerPortStr: string;

  if (parts.length >= 2) {
    // Take the last part (container port)
    containerPortStr = parts[parts.length - 1];
  } else {
    containerPortStr = parts[0];
  }

  // Remove protocol suffix if present (e.g., "80/tcp" -> "80")
  containerPortStr = containerPortStr.split("/")[0];

  const port = parseInt(containerPortStr, 10);
  if (isNaN(port)) {
    throw new Error(`Invalid port mapping: ${firstPort}`);
  }

  return port;
}
