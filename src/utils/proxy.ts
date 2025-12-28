import type { ProxyTarget } from "../lib/configuration/proxy.ts";
import type { SSHManager } from "./ssh.ts";
import { log } from "./logger.ts";
import { executeBestEffort } from "./command_helpers.ts";
import {
  KAMAL_PROXY_CONFIG_VOLUME,
  KAMAL_PROXY_INTERNAL_HTTP_PORT,
  KAMAL_PROXY_INTERNAL_HTTPS_PORT,
} from "../constants.ts";

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
  /** Health check endpoint path (HTTP-based) */
  healthCheckPath?: string;
  /** Health check command (command-based) */
  healthCheckCmd?: string;
  /** Health check command runtime (docker or podman) */
  healthCheckCmdRuntime?: "docker" | "podman";
  /** Health check interval (e.g., "30s", "5m") */
  healthCheckInterval?: string;
  /** Health check timeout (e.g., "5s", "10s") */
  healthCheckTimeout?: string;
  /** Deploy timeout (e.g., "30s", "60s") */
  deployTimeout?: string;
}

/**
 * Builds KamalProxyDeployOptions from a ProxyTarget
 */
export function buildKamalProxyOptionsFromTarget(
  serviceName: string,
  target: ProxyTarget,
  appPort: number,
  projectName: string,
  containerIp?: string,
  defaultRuntime?: "docker" | "podman",
  containerName?: string,
): KamalProxyDeployOptions {
  // Determine target address based on health check type
  let targetAddr: string;

  if (target.healthcheck?.cmd && containerName) {
    // For command-based health checks, use container name so kamal-proxy can exec into it
    targetAddr = `${containerName}:${appPort}`;
  } else if (containerIp) {
    // Use container IP directly if available to avoid DNS caching issues
    targetAddr = `${containerIp}:${appPort}`;
  } else {
    // Fall back to DNS name
    // Extract base service name (remove port suffix if present for DNS lookup)
    const parts = serviceName.split("-");
    const lastPart = parts[parts.length - 1];
    const baseServiceName = /^\d+$/.test(lastPart)
      ? parts.slice(0, -1).join("-")
      : serviceName;
    targetAddr = `${projectName}-${baseServiceName}.jiji:${appPort}`;
  }

  const hosts = target.hosts || (target.host ? [target.host] : undefined);

  // Auto-detect cmd_runtime from builder engine if not specified
  let cmdRuntime = target.healthcheck?.cmd_runtime;
  if (target.healthcheck?.cmd && !cmdRuntime && defaultRuntime) {
    cmdRuntime = defaultRuntime;
  }

  return {
    serviceName,
    target: targetAddr,
    hosts,
    pathPrefix: target.path_prefix,
    tls: target.ssl,
    healthCheckPath: target.healthcheck?.path,
    healthCheckCmd: target.healthcheck?.cmd,
    healthCheckCmdRuntime: cmdRuntime,
    healthCheckInterval: target.healthcheck?.interval,
    healthCheckTimeout: target.healthcheck?.timeout,
    deployTimeout: target.healthcheck?.deploy_timeout,
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

  // HTTP-based health check
  if (options.healthCheckPath) {
    args.push(`--health-check-path=${options.healthCheckPath}`);
  }

  // Command-based health check
  if (options.healthCheckCmd) {
    // Quote the command value if it contains spaces for proper shell parsing
    const cmdValue = options.healthCheckCmd.includes(" ")
      ? `"${options.healthCheckCmd}"`
      : options.healthCheckCmd;
    args.push(`--health-check-cmd=${cmdValue}`);

    // Add runtime if specified, otherwise kamal-proxy defaults to docker
    if (options.healthCheckCmdRuntime) {
      args.push(`--health-check-cmd-runtime=${options.healthCheckCmdRuntime}`);
    }
  }

  // Common health check options (work with both HTTP and command checks)
  if (options.healthCheckInterval) {
    args.push(`--health-check-interval=${options.healthCheckInterval}`);
  }
  if (options.healthCheckTimeout) {
    args.push(`--health-check-timeout=${options.healthCheckTimeout}`);
  }

  if (options.deployTimeout) {
    args.push(`--deploy-timeout=${options.deployTimeout}`);
  }

  log.say(`Generated Kamal Proxy args: ${JSON.stringify(args)}`, 0);

  return args;
}

/**
 * Modern kamal-proxy management for container deployments
 */
export class ProxyCommands {
  private readonly containerName = "kamal-proxy";
  private readonly networkName = "jiji";
  private readonly configVolume = KAMAL_PROXY_CONFIG_VOLUME;
  private readonly internalHttpPort = KAMAL_PROXY_INTERNAL_HTTP_PORT;
  private readonly internalHttpsPort = KAMAL_PROXY_INTERNAL_HTTPS_PORT;

  constructor(
    private engine: "docker" | "podman",
    private ssh: SSHManager,
  ) {}

  async ensureNetwork(): Promise<void> {
    await executeBestEffort(
      this.ssh,
      `${this.engine} network create ${this.networkName}`,
      "creating proxy network",
    );
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
    // const image = "docker.io/basecamp/kamal-proxy:latest";
    const image = "ghcr.io/acidtib/kamal-proxy:jiji";
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

    // Mount container runtime for command-based health checks
    // For podman with --pid=host and --cgroupns=host, we need:
    // - /run for podman socket and runtime state
    // - /usr/bin for podman and helper binaries
    // - /usr/lib for podman network helpers (netavark, aardvark-dns) and shared libraries
    // - /lib* for additional shared libraries
    // - /var/lib/containers for container storage
    // The host namespaces give direct access to processes and cgroups
    // For docker: mount socket only (binary already in image)
    const runtimeMounts = this.engine === "podman"
      ? "--volume /run:/run --volume /usr/bin:/usr/bin:ro --volume /usr/lib:/usr/lib:ro --volume /lib/x86_64-linux-gnu:/lib/x86_64-linux-gnu:ro --volume /lib64:/lib64:ro --volume /var/lib/containers:/var/lib/containers"
      : "--volume /var/run/docker.sock:/var/run/docker.sock";

    // For podman command health checks to work, we need:
    // - privileged mode for namespace operations
    // - root user so podman commands have proper permissions
    // - host PID namespace so kamal-proxy can see and exec into other container processes
    // - host cgroup namespace so kamal-proxy can access container cgroups in /sys
    const privilegedFlag = this.engine === "podman"
      ? "--privileged --user root --pid=host --cgroupns=host"
      : "";

    const command =
      `${this.engine} run --name ${this.containerName} --network ${this.networkName} ${dnsArgs} --detach --restart unless-stopped ${privilegedFlag} --volume ${this.configVolume}:/home/kamal-proxy/.config/kamal-proxy ${runtimeMounts} ${publishArgs} ghcr.io/acidtib/kamal-proxy:jiji kamal-proxy run --http-port ${this.internalHttpPort} --https-port ${this.internalHttpsPort}`;

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
    await executeBestEffort(
      this.ssh,
      `${this.engine} container rm -f ${this.containerName}`,
      "removing kamal-proxy container",
    );
  }

  async removeImage(): Promise<void> {
    await executeBestEffort(
      this.ssh,
      `${this.engine} image rm -f basecamp/kamal-proxy`,
      "removing kamal-proxy image",
    );
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

  /**
   * Deploy a specific proxy target
   */
  async deployTarget(
    serviceName: string,
    containerName: string,
    target: ProxyTarget,
    appPort: number,
    projectName: string,
    containerIp?: string,
  ): Promise<void> {
    const options = buildKamalProxyOptionsFromTarget(
      serviceName,
      target,
      appPort,
      projectName,
      containerIp,
      this.engine, // Pass engine as default runtime for command health checks
      containerName, // Pass container name for command-based health checks
    );

    const args = buildDeployCommandArgs(options);
    const argsStr = args.join(" ");

    log.debug(
      `Deploying ${serviceName} to proxy with target: ${options.target}`,
      "proxy",
    );

    const command =
      `${this.engine} exec ${this.containerName} kamal-proxy deploy ${serviceName} ${argsStr}`;

    const result = await this.ssh.executeCommand(command);
    if (!result.success) {
      throw new Error(
        `Failed to deploy service ${serviceName} to proxy: ${
          result.stderr || result.stdout
        }`,
      );
    }
  }

  async remove(service: string): Promise<void> {
    await executeBestEffort(
      this.ssh,
      `${this.engine} exec ${this.containerName} kamal-proxy remove ${service}`,
      `removing service ${service} from proxy`,
    );
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

  /**
   * Get detailed information about all services in kamal-proxy
   * Returns a map of service name to proxy details
   */
  async getServiceDetails(): Promise<
    Map<
      string,
      {
        host: string;
        path: string;
        target: string;
        state: string;
        tls: boolean;
      }
    >
  > {
    // Disable color output by setting NO_COLOR environment variable
    const command =
      `NO_COLOR=1 ${this.engine} exec ${this.containerName} kamal-proxy list`;
    const result = await this.ssh.executeCommand(command);

    const serviceMap = new Map<
      string,
      {
        host: string;
        path: string;
        target: string;
        state: string;
        tls: boolean;
      }
    >();

    if (!result.success) {
      return serviceMap;
    }

    // Helper function to strip ANSI color codes
    const stripAnsi = (str: string): string => {
      // deno-lint-ignore no-control-regex
      return str.replace(/\x1b\[[0-9;]*m/g, "");
    };

    const lines = result.stdout.split("\n").map((line) =>
      stripAnsi(line.trim())
    );

    // Skip the header line and empty lines
    for (const line of lines) {
      if (!line || line.startsWith("Service") || line.startsWith("---")) {
        continue;
      }

      // Parse the table format: Service  Host  Path  Target  State  TLS
      // Split by multiple spaces to handle column alignment
      const parts = line.split(/\s{2,}/).map((p) => p.trim());

      if (parts.length >= 6) {
        const [service, host, path, target, state, tlsStr] = parts;
        serviceMap.set(service, {
          host,
          path,
          target,
          state,
          tls: tlsStr.toLowerCase() === "yes",
        });
      }
    }

    return serviceMap;
  }

  async info(): Promise<string> {
    const command = `${this.engine} ps --filter "name=^${this.containerName}$"`;
    const result = await this.ssh.executeCommand(command);
    return result.stdout;
  }
}
