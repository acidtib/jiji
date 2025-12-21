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
  /** Host domains for routing (can be multiple) */
  hosts?: string[];
  /** Path prefix for path-based routing */
  pathPrefix?: string;
  /** Enable TLS/SSL */
  tls?: boolean;
  /** Health check endpoint path */
  healthCheckPath?: string;
  /** Health check interval (e.g., "30s", "5m") */
  healthCheckInterval?: string;
  /** Health check timeout (e.g., "5s", "10s") */
  healthCheckTimeout?: string;
  /** Deploy timeout (e.g., "30s", "60s") */
  deployTimeout?: string;
}

/**
 * Converts ProxyConfiguration to KamalProxyDeployOptions
 */
export function buildKamalProxyOptions(
  serviceName: string,
  _containerName: string,
  appPort: number,
  config: ProxyConfiguration,
  projectName: string,
): KamalProxyDeployOptions {
  return {
    serviceName,
    target: `${projectName}-${serviceName}.jiji:${appPort}`,
    hosts: config.hosts.length > 0 ? config.hosts : undefined,
    pathPrefix: config.pathPrefix,
    tls: config.ssl,
    healthCheckPath: config.healthcheck?.path,
    healthCheckInterval: config.healthcheck?.interval,
    healthCheckTimeout: config.healthcheck?.timeout,
    deployTimeout: config.healthcheck?.deploy_timeout,
  };
}

/**
 * Builds kamal-proxy deploy command arguments from options
 */
export function buildDeployCommandArgs(
  options: KamalProxyDeployOptions,
): string[] {
  const args: string[] = [`--target=${options.target}`];

  // Add multiple hosts (kamal-proxy supports --host flag multiple times)
  if (options.hosts && options.hosts.length > 0) {
    for (const host of options.hosts) {
      args.push(`--host=${host}`);
    }
  }

  if (options.pathPrefix) args.push(`--path-prefix=${options.pathPrefix}`);
  if (options.tls) args.push("--tls");
  if (options.healthCheckPath) {
    args.push(`--health-check-path=${options.healthCheckPath}`);
  }
  if (options.healthCheckInterval) {
    args.push(`--health-check-interval=${options.healthCheckInterval}`);
  }
  if (options.healthCheckTimeout) {
    args.push(`--health-check-timeout=${options.healthCheckTimeout}`);
  }
  if (options.deployTimeout) {
    args.push(`--deploy-timeout=${options.deployTimeout}`);
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
    dnsServer?: string;
  }): Promise<void> {
    const {
      publish = true,
      httpPort = 80,
      httpsPort = 443,
      dnsServer,
    } = options || {};

    // Pull the image first
    await this.pullImage();

    // Remove existing container if it exists
    await this.removeContainer();

    const publishArgs = publish
      ? `-p ${httpPort}:${this.internalHttpPort} -p ${httpsPort}:${this.internalHttpsPort}`
      : "";

    // For Podman, explicitly set DNS to ensure service discovery works
    // For Docker, daemon.json handles this but explicit --dns doesn't hurt
    const dnsArgs = dnsServer
      ? `--dns ${dnsServer} --dns 8.8.8.8 --dns-search jiji --dns-option ndots:1`
      : "";

    const command =
      `${this.engine} run --name ${this.containerName} --network ${this.networkName} ${dnsArgs} --detach --restart unless-stopped --volume ${this.configVolume}:/home/kamal-proxy/.config/kamal-proxy ${publishArgs} docker.io/basecamp/kamal-proxy:latest kamal-proxy run --http-port ${this.internalHttpPort} --https-port ${this.internalHttpsPort}`;

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
    dnsServer?: string;
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

  async refreshDNS(): Promise<void> {
    // Trigger immediate DNS hosts update
    const updateCommand = `/opt/jiji/dns/update-hosts.sh`;
    const updateResult = await this.ssh.executeCommand(updateCommand);

    if (!updateResult.success) {
      log.warn(`DNS update failed: ${updateResult.stderr}`, "proxy");
    } else {
      log.debug("DNS hosts updated before proxy deployment", "proxy");
    }

    // Restart kamal-proxy to pick up fresh DNS entries
    log.debug("Refreshing kamal-proxy DNS resolution...", "proxy");
    await this.restart();

    // Wait for proxy to be ready after restart
    await this.waitForReady();
  }

  async restart(): Promise<void> {
    const restartCommand = `${this.engine} restart ${this.containerName}`;
    const result = await this.ssh.executeCommand(restartCommand);

    if (!result.success) {
      throw new Error(
        `Failed to restart kamal-proxy: ${result.stderr || result.stdout}`,
      );
    }
  }

  async deploy(
    service: string,
    containerName: string,
    config: ProxyConfiguration,
    appPort: number,
    projectName: string,
  ): Promise<void> {
    // Refresh DNS before deployment to ensure fresh hostname resolution
    await this.refreshDNS();

    const options = buildKamalProxyOptions(
      service,
      containerName,
      appPort,
      config,
      projectName,
    );

    const args = buildDeployCommandArgs(options);
    const argsStr = args.join(" ");

    log.debug(
      `Deploying ${service} to proxy with target: ${options.target}`,
      "proxy",
    );

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

  // Remove protocol suffix if present
  const portWithoutProtocol = firstPort.replace(/(\/tcp|\/udp)$/, "");
  const parts = portWithoutProtocol.split(":");

  let containerPortStr: string;

  if (parts.length === 1) {
    // Format: "8000" (container port only)
    containerPortStr = parts[0];
  } else if (parts.length === 2) {
    // Format: "8080:8000" (host_port:container_port)
    containerPortStr = parts[1];
  } else if (parts.length === 3) {
    // Format: "192.168.1.1:8080:8000" (host_ip:host_port:container_port)
    containerPortStr = parts[2];
  } else {
    throw new Error(`Invalid port mapping format: ${firstPort}`);
  }

  const port = parseInt(containerPortStr, 10);
  if (isNaN(port)) {
    throw new Error(`Invalid port mapping: ${firstPort}`);
  }

  return port;
}
