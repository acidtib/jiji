/**
 * Service for deploying containers to remote servers
 */

import type { ContainerEngine } from "../configuration/builder.ts";
import type { Configuration } from "../configuration.ts";
import type { ServiceConfiguration } from "../configuration/service.ts";
import type { SSHManager } from "../../utils/ssh.ts";
import { ContainerRunBuilder } from "./container_run_builder.ts";
import {
  cleanupServiceContainers,
  getContainerIp,
  registerContainerClusterWide,
  registerContainerInNetwork,
} from "./container_registry.ts";
import {
  buildAllMountArgs,
  prepareMountDirectories,
  prepareMountFiles,
} from "../../utils/mount_manager.ts";
import { getDnsServerForHost } from "../../utils/network_helpers.ts";
import { getServerByHostname, loadTopology } from "../network/topology.ts";
import { RegistryAuthService } from "./registry_auth_service.ts";
import { log } from "../../utils/logger.ts";
import {
  CONTAINER_LOG_TAIL_LINES,
  CONTAINER_START_MAX_ATTEMPTS,
  CONTAINER_START_RETRY_DELAY_MS,
  JIJI_NETWORK_NAME,
} from "../../constants.ts";

/**
 * Options for container deployment
 */
export interface DeploymentOptions {
  /**
   * Custom version tag (defaults to "latest")
   */
  version?: string;
  /**
   * All SSH managers for cluster-wide operations
   */
  allSshManagers?: SSHManager[];
}

/**
 * Result of container deployment
 */
export interface DeploymentResult {
  service: string;
  host: string;
  success: boolean;
  containerName?: string;
  imageName?: string;
  containerIp?: string;
  error?: string;
}

/**
 * Service for deploying containers to remote servers
 */
export class ContainerDeploymentService {
  private registryAuthService: RegistryAuthService;

  constructor(
    private engine: ContainerEngine,
    private config: Configuration,
  ) {
    this.registryAuthService = new RegistryAuthService(
      engine,
      config.builder.registry,
    );
  }

  /**
   * Deploy a service container to a specific host
   *
   * @param service Service configuration
   * @param host Hostname
   * @param ssh SSH manager for the host
   * @param options Deployment options
   * @returns Deployment result
   */
  async deployService(
    service: ServiceConfiguration,
    host: string,
    ssh: SSHManager,
    options: DeploymentOptions = {},
  ): Promise<DeploymentResult> {
    try {
      const containerName = service.getContainerName();
      const version = options.version || "latest";

      // Determine image name with optional version and registry
      const imageName = service.requiresBuild()
        ? this.config.builder.registry.getFullImageName(
          service.project,
          service.name,
          version,
        )
        : service.getImageName(undefined, version);

      log.status(`Deploying ${service.name} on ${host}`, "deploy");

      // Upload files
      await this.uploadFiles(service, ssh, host);

      // Create directories
      await this.createDirectories(service, ssh, host);

      // Authenticate to registry if needed
      if (!this.config.builder.registry.isLocal() && service.requiresBuild()) {
        await this.registryAuthService.authenticateRemotely(ssh);
      }

      // Pull image
      const fullImageName = await this.pullImage(imageName, ssh, host);

      // Clean up old containers from network registry
      if (this.config.network.enabled) {
        await this.cleanupOldContainers(service, ssh);
      }

      // Stop and remove existing container
      await ssh.executeCommand(
        `${this.engine} rm -f ${containerName} 2>/dev/null || true`,
      );

      // Start new container
      const containerId = await this.startContainer(
        service,
        fullImageName,
        containerName,
        ssh,
        host,
      );

      // Wait for container to be running
      await this.waitForContainerRunning(containerName, ssh);

      // Register in network if enabled
      let containerIp: string | undefined;
      if (this.config.network.enabled) {
        containerIp = await this.registerInNetwork(
          service,
          containerName,
          host,
          ssh,
          options.allSshManagers,
        );
      }

      log.success(`${service.name} deployed successfully on ${host}`, "deploy");

      return {
        service: service.name,
        host,
        success: true,
        containerName,
        imageName: fullImageName,
        containerIp,
      };
    } catch (error) {
      const errorMessage = error instanceof Error
        ? error.message
        : String(error);
      log.error(
        `Failed to deploy ${service.name} on ${host}: ${errorMessage}`,
        "deploy",
      );

      return {
        service: service.name,
        host,
        success: false,
        error: errorMessage,
      };
    }
  }

  /**
   * Deploy a service to all its configured servers
   *
   * @param service Service configuration
   * @param sshManagers All SSH managers
   * @param connectedHosts List of connected hosts
   * @param options Deployment options
   * @returns Array of deployment results
   */
  async deployServiceToServers(
    service: ServiceConfiguration,
    sshManagers: SSHManager[],
    connectedHosts: string[],
    options: DeploymentOptions = {},
  ): Promise<DeploymentResult[]> {
    const results: DeploymentResult[] = [];

    for (const server of service.servers) {
      const host = server.host;

      if (!connectedHosts.includes(host)) {
        log.warn(
          `Skipping ${service.name} on unreachable host: ${host}`,
          "deploy",
        );
        results.push({
          service: service.name,
          host,
          success: false,
          error: "Host not connected",
        });
        continue;
      }

      const hostSsh = sshManagers.find((ssh) => ssh.getHost() === host);
      if (!hostSsh) {
        results.push({
          service: service.name,
          host,
          success: false,
          error: "SSH manager not found",
        });
        continue;
      }

      const result = await this.deployService(service, host, hostSsh, {
        ...options,
        allSshManagers: sshManagers,
      });
      results.push(result);
    }

    return results;
  }

  /**
   * Deploy multiple services
   *
   * @param services Services to deploy
   * @param sshManagers All SSH managers
   * @param connectedHosts List of connected hosts
   * @param options Deployment options
   * @returns Array of deployment results
   */
  async deployServices(
    services: ServiceConfiguration[],
    sshManagers: SSHManager[],
    connectedHosts: string[],
    options: DeploymentOptions = {},
  ): Promise<DeploymentResult[]> {
    const allResults: DeploymentResult[] = [];

    for (const service of services) {
      log.status(`Deploying ${service.name} containers`, "deploy");
      const results = await this.deployServiceToServers(
        service,
        sshManagers,
        connectedHosts,
        options,
      );
      allResults.push(...results);
    }

    return allResults;
  }

  /**
   * Upload files for a service
   */
  private async uploadFiles(
    service: ServiceConfiguration,
    ssh: SSHManager,
    host: string,
  ): Promise<void> {
    if (service.files.length === 0) {
      return;
    }

    log.status(
      `Uploading ${service.files.length} file(s) for ${service.name} on ${host}`,
      "deploy",
    );

    try {
      await prepareMountFiles(ssh, service.files, this.config.project);
      log.success(`Files uploaded for ${service.name} on ${host}`, "deploy");
    } catch (error) {
      const errorMessage = error instanceof Error
        ? error.message
        : String(error);
      log.error(`Failed to upload files: ${errorMessage}`, "deploy");
      throw error;
    }
  }

  /**
   * Create directories for a service
   */
  private async createDirectories(
    service: ServiceConfiguration,
    ssh: SSHManager,
    host: string,
  ): Promise<void> {
    if (service.directories.length === 0) {
      return;
    }

    log.status(
      `Creating ${service.directories.length} director(ies) for ${service.name} on ${host}`,
      "deploy",
    );

    try {
      await prepareMountDirectories(
        ssh,
        service.directories,
        this.config.project,
      );
      log.success(
        `Directories created for ${service.name} on ${host}`,
        "deploy",
      );
    } catch (error) {
      const errorMessage = error instanceof Error
        ? error.message
        : String(error);
      log.error(`Failed to create directories: ${errorMessage}`, "deploy");
      throw error;
    }
  }

  /**
   * Pull container image
   */
  private async pullImage(
    imageName: string,
    ssh: SSHManager,
    host: string,
  ): Promise<string> {
    // For Podman, ensure image has full registry path
    const fullImageName = imageName.includes("/")
      ? imageName
      : `docker.io/library/${imageName}`;

    // Build pull command with TLS verification disabled for local registries
    let pullCommand = `${this.engine} pull`;

    // Add --tls-verify=false for local registries when using podman
    if (
      this.config.builder.registry.isLocal() &&
      this.engine === "podman"
    ) {
      pullCommand += " --tls-verify=false";
    }

    pullCommand += ` ${fullImageName}`;

    // Pull image
    log.status(`Pulling image ${fullImageName} on ${host}`, "deploy");
    const pullResult = await ssh.executeCommand(pullCommand);

    if (!pullResult.success) {
      throw new Error(`Failed to pull image: ${pullResult.stderr}`);
    }

    return fullImageName;
  }

  /**
   * Clean up old service containers
   */
  private async cleanupOldContainers(
    service: ServiceConfiguration,
    ssh: SSHManager,
  ): Promise<void> {
    try {
      const cleanedCount = await cleanupServiceContainers(
        ssh,
        service.name,
        this.engine,
        this.config.project,
      );
      if (cleanedCount > 0) {
        log.debug(
          `Cleaned up ${cleanedCount} stale containers for ${service.name}`,
          "network",
        );
      }
    } catch (error) {
      log.warn(
        `Service cleanup failed: ${error} (deployment will continue)`,
        "network",
      );
    }
  }

  /**
   * Start a container
   */
  private async startContainer(
    service: ServiceConfiguration,
    fullImageName: string,
    containerName: string,
    ssh: SSHManager,
    host: string,
  ): Promise<string> {
    // Build container run command
    const mountArgs = buildAllMountArgs(
      service.files,
      service.directories,
      service.volumes,
      this.config.project,
    );
    const mergedEnv = service.getMergedEnvironment();
    const envArray = mergedEnv.toEnvArray();

    // Get DNS server from network topology
    const dnsServer = await getDnsServerForHost(
      ssh,
      host,
      this.config.network.enabled,
    );

    const builder = new ContainerRunBuilder(
      this.engine,
      containerName,
      fullImageName,
    )
      .network(JIJI_NETWORK_NAME)
      .detached()
      .restart("unless-stopped")
      .ports(service.ports)
      .volumes(mountArgs)
      .environment(envArray);

    // Add DNS configuration if network is enabled
    if (dnsServer) {
      builder.dns(dnsServer, this.config.network.serviceDomain);
    }

    const runCommand = builder.build();

    log.status(`Starting container ${containerName} on ${host}`, "deploy");
    const runResult = await ssh.executeCommand(runCommand);

    if (!runResult.success) {
      throw new Error(`Failed to start container: ${runResult.stderr}`);
    }

    return runResult.stdout.trim();
  }

  /**
   * Wait for container to be running
   */
  private async waitForContainerRunning(
    containerName: string,
    ssh: SSHManager,
  ): Promise<void> {
    let attempts = 0;

    while (attempts < CONTAINER_START_MAX_ATTEMPTS) {
      const statusResult = await ssh.executeCommand(
        `${this.engine} inspect ${containerName} --format '{{.State.Status}}'`,
      );

      if (
        statusResult.success &&
        statusResult.stdout.trim() === "running"
      ) {
        return;
      }

      attempts++;
      await new Promise((resolve) =>
        setTimeout(resolve, CONTAINER_START_RETRY_DELAY_MS)
      );
    }

    // Get container logs for debugging
    const logsCmd =
      `${this.engine} logs --tail ${CONTAINER_LOG_TAIL_LINES} ${containerName} 2>&1`;
    const logsResult = await ssh.executeCommand(logsCmd);

    throw new Error(
      `Container ${containerName} did not start within ${CONTAINER_START_MAX_ATTEMPTS} seconds. Logs: ${logsResult.stdout}`,
    );
  }

  /**
   * Register container in network
   */
  private async registerInNetwork(
    service: ServiceConfiguration,
    containerName: string,
    host: string,
    ssh: SSHManager,
    allSshManagers?: SSHManager[],
  ): Promise<string | undefined> {
    try {
      // Load topology from Corrosion via SSH
      const topology = await loadTopology(ssh);
      if (!topology) {
        log.warn(
          `Network cluster not initialized - skipping network registration`,
          "network",
        );
        return undefined;
      }

      const server = getServerByHostname(topology, host);
      if (!server) {
        log.warn(`Server ${host} not found in network topology`, "network");
        return undefined;
      }

      log.status(`Registering ${service.name} in network...`, "network");

      // First register locally (this gets IP and sets up DNS)
      const registered = await registerContainerInNetwork(
        ssh,
        service.name,
        this.config.project,
        server.id,
        containerName,
        this.engine,
      );

      if (!registered) {
        log.warn(
          `Failed to register ${service.name} in network (service will still run)`,
          "network",
        );
        return undefined;
      }

      // Get container IP for cluster-wide registration
      const containerIp = await getContainerIp(
        ssh,
        containerName,
        this.engine,
      );

      if (containerIp && allSshManagers) {
        // Register this container on all servers for DNS resolution
        await registerContainerClusterWide(
          allSshManagers,
          service.name,
          this.config.project,
          server.id,
          containerName,
          containerIp,
          Date.now(),
        );
        log.debug(
          `Registered ${service.name} cluster-wide for DNS resolution`,
          "network",
        );
      }

      return containerIp || undefined;
    } catch (error) {
      log.warn(
        `Network registration failed: ${error} (service will still run)`,
        "network",
      );
      return undefined;
    }
  }
}
