import type { ProxyConfiguration } from "../lib/configuration/proxy.ts";
import type { SSHManager } from "./ssh.ts";
import { log } from "./logger.ts";

/**
 * Options for deploying a service to kamal-proxy
 */
export interface KamalProxyDeployOptions {
  /** Service name to deploy */
  serviceName: string;
  /** Target container name and port (containerName:port) */
  target: string;
  /** Host domain for routing */
  host?: string;
  /** Path prefix for path-based routing */
  pathPrefix?: string;
  /** Enable TLS/SSL */
  tls?: boolean;
  /** Health check endpoint path */
  healthCheckPath?: string;
  /** Health check interval (e.g., "30s", "5m") */
  healthCheckInterval?: string;
}

/**
 * Converts ProxyConfiguration to KamalProxyDeployOptions
 */
export function buildKamalProxyOptions(
  serviceName: string,
  containerName: string,
  appPort: number,
  config: ProxyConfiguration,
): KamalProxyDeployOptions {
  return {
    serviceName,
    target: `${containerName}:${appPort}`,
    host: config.host,
    pathPrefix: config.pathPrefix,
    tls: config.ssl,
    healthCheckPath: config.healthcheck?.path,
    healthCheckInterval: config.healthcheck?.interval,
  };
}

/**
 * Builds kamal-proxy deploy command arguments from options
 */
export function buildDeployCommandArgs(
  options: KamalProxyDeployOptions,
): string[] {
  const args: string[] = [`--target=${options.target}`];

  if (options.host) args.push(`--host=${options.host}`);
  if (options.pathPrefix) args.push(`--path-prefix=${options.pathPrefix}`);
  if (options.tls) args.push("--tls");
  if (options.healthCheckPath) {
    args.push(`--health-check-path=${options.healthCheckPath}`);
  }
  if (options.healthCheckInterval) {
    args.push(`--health-check-interval=${options.healthCheckInterval}`);
  }

  return args;
}

/**
 * Modern kamal-proxy management for container deployments
 */
export class ProxyCommands {
  private readonly containerName = "kamal-proxy";
  private readonly networkName = "jiji";
  private readonly configVolume = "kamal-proxy-config";
  private readonly internalHttpPort = 8080;
  private readonly internalHttpsPort = 8443;

  constructor(
    private engine: "docker" | "podman",
    private ssh: SSHManager,
  ) {}

  async ensureNetwork(): Promise<void> {
    const command =
      `${this.engine} network create ${this.networkName} 2>/dev/null || true`;
    await this.ssh.executeCommand(command);
  }

  async isRunning(): Promise<boolean> {
    const command =
      `${this.engine} ps --filter "name=^${this.containerName}$" --format "{{.Names}}" | grep -q "${this.containerName}"`;
    const result = await this.ssh.executeCommand(command);
    return result.success;
  }

  async getVersion(): Promise<string | null> {
    const command =
      `${this.engine} inspect ${this.containerName} --format '{{.Config.Image}}' 2>/dev/null | awk -F: '{print $NF}'`;
    const result = await this.ssh.executeCommand(command);
    return result.success ? result.stdout.trim() : null;
  }

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

    await new Promise((resolve) => setTimeout(resolve, 500));
  }

  async start(): Promise<void> {
    const command = `${this.engine} start ${this.containerName}`;
    const result = await this.ssh.executeCommand(command);
    if (!result.success) {
      throw new Error(
        `Failed to start kamal-proxy: ${result.stderr || result.stdout}`,
      );
    }
  }

  async stop(): Promise<void> {
    const command = `${this.engine} stop ${this.containerName}`;
    await this.ssh.executeCommand(command);
  }

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

    await this.waitForReady();
  }

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

      if (i % 5 === 0 && result.success) {
        log.info(
          `Waiting for kamal-proxy... (status: ${result.stdout.trim()})`,
          "proxy",
        );
      }

      await new Promise((resolve) => setTimeout(resolve, delayMs));
    }

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

  async removeContainer(): Promise<void> {
    const command =
      `${this.engine} container rm -f ${this.containerName} 2>/dev/null || true`;
    await this.ssh.executeCommand(command);
  }

  async removeImage(): Promise<void> {
    const command =
      `${this.engine} image rm -f basecamp/kamal-proxy 2>/dev/null || true`;
    await this.ssh.executeCommand(command);
  }

  async deploy(
    service: string,
    containerName: string,
    config: ProxyConfiguration,
    appPort: number,
  ): Promise<void> {
    const options = buildKamalProxyOptions(
      service,
      containerName,
      appPort,
      config,
    );

    const args = buildDeployCommandArgs(options);
    const argsStr = args.join(" ");

    const command =
      `${this.engine} exec ${this.containerName} kamal-proxy deploy ${service} ${argsStr}`;

    const result = await this.ssh.executeCommand(command);
    if (!result.success) {
      throw new Error(
        `Failed to deploy service ${service} to proxy: ${
          result.stderr || result.stdout
        }`,
      );
    }
  }

  async remove(service: string): Promise<void> {
    const command =
      `${this.engine} exec ${this.containerName} kamal-proxy remove ${service} 2>/dev/null || true`;
    await this.ssh.executeCommand(command);
  }

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

  async info(): Promise<string> {
    const command = `${this.engine} ps --filter "name=^${this.containerName}$"`;
    const result = await this.ssh.executeCommand(command);
    return result.stdout;
  }
}

export function extractAppPort(ports: string[]): number {
  if (ports.length === 0) {
    return 3000; // Default app port
  }

  const firstPort = ports[0];
  const parts = firstPort.split(":");
  const containerPortStr = parts.length >= 2
    ? parts[parts.length - 1].split("/")[0]
    : parts[0].split("/")[0];

  const port = parseInt(containerPortStr, 10);
  if (isNaN(port)) {
    throw new Error(`Invalid port mapping: ${firstPort}`);
  }

  return port;
}
