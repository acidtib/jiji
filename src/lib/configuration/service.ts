import {
  BaseConfiguration,
  ConfigurationError,
  type Validatable,
} from "./base.ts";
import { ProxyConfiguration } from "./proxy.ts";
import { EnvironmentConfiguration } from "./environment.ts";

/**
 * Build configuration for a service
 */
export interface BuildConfig {
  context: string;
  dockerfile?: string;
  args?: Record<string, string>;
  target?: string;
}

/**
 * Server configuration
 */
export type ServerConfig = {
  host: string;
  arch?: string;
  alias?: string; // Optional human-friendly identifier for instance-specific domains
};

/**
 * Mount options for files and directories
 */
type MountOptions = "ro" | "z" | "Z";

/**
 * Mount configuration - supports both string and hash formats
 * String format: "local:remote" or "local:remote:options"
 * Hash format: { local: string, remote: string, mode?: string, owner?: string, options?: string }
 */
export type MountConfig = string | {
  local: string;
  remote: string;
  mode?: string;
  owner?: string;
  options?: MountOptions | string;
};

/**
 * File mount configuration
 */
export type FileMountConfig = MountConfig;

/**
 * Directory mount configuration
 */
export type DirectoryMountConfig = MountConfig;

/**
 * Service configuration representing a single deployable service
 */
export class ServiceConfiguration extends BaseConfiguration
  implements Validatable {
  private _name: string;
  private _project: string;
  private _image?: string;
  private _build?: string | BuildConfig;
  private _servers?: ServerConfig[];
  private _ports?: string[];
  private _volumes?: string[];
  private _files?: FileMountConfig[];
  private _directories?: DirectoryMountConfig[];
  private _environment?: EnvironmentConfiguration;
  private _sharedEnvironment?: EnvironmentConfiguration;
  private _command?: string | string[];
  private _proxy?: ProxyConfiguration;
  private _retain?: number;
  private _network_mode?: string;

  constructor(
    name: string,
    config: Record<string, unknown>,
    project: string,
    sharedEnvironment?: EnvironmentConfiguration,
  ) {
    super(config);
    this._name = name;
    this._project = project;
    this._sharedEnvironment = sharedEnvironment;
  }

  /**
   * Service name
   */
  get name(): string {
    return this._name;
  }

  /**
   * Project name
   */
  get project(): string {
    return this._project;
  }

  /**
   * Docker/Podman image to use
   */
  get image(): string | undefined {
    if (!this._image && this.has("image")) {
      this._image = this.validateString(this.get("image"), "image", this.name);
    }
    return this._image;
  }

  /**
   * Build configuration (either a string context path or build config object)
   */
  get build(): string | BuildConfig | undefined {
    if (!this._build && this.has("build")) {
      const buildValue = this.get("build");
      if (typeof buildValue === "string") {
        this._build = buildValue;
      } else if (typeof buildValue === "object" && buildValue !== null) {
        const buildObj = buildValue as Record<string, unknown>;
        this._build = {
          context: this.validateString(
            buildObj.context,
            "build.context",
            this.name,
          ),
          dockerfile: buildObj.dockerfile
            ? this.validateString(
              buildObj.dockerfile,
              "build.dockerfile",
              this.name,
            )
            : undefined,
          args: buildObj.args
            ? this.validateObject(
              buildObj.args,
              "build.args",
              this.name,
            ) as Record<string, string>
            : undefined,
          target: buildObj.target
            ? this.validateString(buildObj.target, "build.target", this.name)
            : undefined,
        };
      } else {
        throw new ConfigurationError(
          `'build' for service '${this.name}' must be a string or object`,
        );
      }
    }
    return this._build;
  }

  /**
   * List of servers to deploy to
   */
  get servers(): ServerConfig[] {
    if (!this._servers) {
      this._servers = this.has("servers")
        ? this.validateServers(this.get("servers"))
        : [];
    }
    return this._servers;
  }

  /**
   * Port mappings
   */
  get ports(): string[] {
    if (!this._ports) {
      this._ports = this.has("ports")
        ? this.validateArray<string>(this.get("ports"), "ports", this.name)
        : [];
    }
    return this._ports;
  }

  /**
   * Volume mounts
   */
  get volumes(): string[] {
    if (!this._volumes) {
      this._volumes = this.has("volumes")
        ? this.validateArray<string>(this.get("volumes"), "volumes", this.name)
        : [];
    }
    return this._volumes;
  }

  /**
   * File mounts
   */
  get files(): FileMountConfig[] {
    if (!this._files) {
      this._files = this.has("files")
        ? this.validateArray<FileMountConfig>(
          this.get("files"),
          "files",
          this.name,
        )
        : [];
    }
    return this._files;
  }

  /**
   * Directory mounts
   */
  get directories(): DirectoryMountConfig[] {
    if (!this._directories) {
      this._directories = this.has("directories")
        ? this.validateArray<DirectoryMountConfig>(
          this.get("directories"),
          "directories",
          this.name,
        )
        : [];
    }
    return this._directories;
  }

  /**
   * Service-specific environment configuration
   */
  get environment(): EnvironmentConfiguration {
    if (!this._environment) {
      if (this.has("environment")) {
        const envValue = this.get("environment");
        if (typeof envValue === "object" && envValue !== null) {
          this._environment = new EnvironmentConfiguration(
            envValue as Record<string, unknown>,
          );
        } else {
          throw new ConfigurationError(
            `'environment' for service '${this.name}' must be an object`,
          );
        }
      } else {
        this._environment = EnvironmentConfiguration.empty();
      }
    }
    return this._environment;
  }

  /**
   * Get merged environment (shared + service-specific)
   */
  getMergedEnvironment(): EnvironmentConfiguration {
    if (this._sharedEnvironment) {
      return this._sharedEnvironment.merge(this.environment);
    }
    return this.environment;
  }

  /**
   * Container command
   */
  get command(): string | string[] | undefined {
    if (!this._command && this.has("command")) {
      const cmdValue = this.get("command");
      if (typeof cmdValue === "string" || Array.isArray(cmdValue)) {
        this._command = cmdValue as string | string[];
      } else {
        throw new ConfigurationError(
          `'command' for service '${this.name}' must be a string or array`,
        );
      }
    }
    return this._command;
  }

  /**
   * Proxy configuration
   */
  get proxy(): ProxyConfiguration | undefined {
    if (!this._proxy && this.has("proxy")) {
      const proxyValue = this.get("proxy");
      if (typeof proxyValue === "object" && proxyValue !== null) {
        this._proxy = new ProxyConfiguration(
          proxyValue as Record<string, unknown>,
        );
      } else {
        throw new ConfigurationError(
          `'proxy' for service '${this.name}' must be an object`,
        );
      }
    }
    return this._proxy;
  }

  /**
   * Number of images to retain for this service (default: 3)
   */
  get retain(): number {
    if (this._retain === undefined && this.has("retain")) {
      const retainValue = this.get("retain");
      if (typeof retainValue === "number" && retainValue > 0) {
        this._retain = retainValue;
      } else {
        throw new ConfigurationError(
          `'retain' for service '${this.name}' must be a positive number`,
        );
      }
    }
    return this._retain ?? 3; // Default to 3 if not specified
  }

  /**
   * Network mode for the container (default: "bridge")
   */
  get network_mode(): string {
    if (!this._network_mode && this.has("network_mode")) {
      const networkModeValue = this.get("network_mode");
      if (typeof networkModeValue === "string") {
        this._network_mode = networkModeValue;
      } else {
        throw new ConfigurationError(
          `'network_mode' for service '${this.name}' must be a string`,
        );
      }
    }
    return this._network_mode ?? "bridge"; // Default to "bridge" if not specified
  }

  /**
   * Validates the service configuration
   */
  validate(): void {
    // Must have either image or build
    if (!this.image && !this.build) {
      throw new ConfigurationError(
        `Service '${this.name}' must specify either 'image' or 'build'`,
      );
    }

    // Cannot have both image and build
    if (this.image && this.build) {
      throw new ConfigurationError(
        `Service '${this.name}' cannot specify both 'image' and 'build'`,
      );
    }

    // Validate build configuration if present
    if (this.build && typeof this.build === "object") {
      if (!this.build.context) {
        throw new ConfigurationError(
          `Service '${this.name}' build configuration must specify 'context'`,
        );
      }
    }

    // Validate ports format
    for (const port of this.ports) {
      if (!this.isValidPortMapping(port)) {
        throw new ConfigurationError(
          `Invalid port mapping '${port}' for service '${this.name}'. Expected format: container_port, host_port:container_port, or [host_ip:]host_port:container_port[/protocol]`,
        );
      }
    }

    // Validate volume format
    for (const volume of this.volumes) {
      if (!this.isValidVolumeMapping(volume)) {
        throw new ConfigurationError(
          `Invalid volume mapping '${volume}' for service '${this.name}'. Expected format: host_path:container_path[:options]`,
        );
      }
    }

    this.validateMounts(this.files, "file");
    this.validateMounts(this.directories, "directory");

    this.environment.validate();

    if (this.servers.length === 0) {
      throw new ConfigurationError(
        `Service '${this.name}' must specify at least one server`,
      );
    }

    for (const serverConfig of this.servers) {
      this.validateHost(serverConfig.host, "servers", this.name);
      if (serverConfig.arch) {
        this.validateArch(serverConfig.arch);
      }
    }

    if (this.proxy) {
      this.proxy.validate();

      // Validate that proxy target ports exist in service ports
      for (const target of this.proxy.targets) {
        if (!this.hasPort(target.app_port)) {
          throw new ConfigurationError(
            `Proxy target app_port ${target.app_port} not found in service '${this.name}' ports. ` +
              `Available ports: ${this.ports.join(", ")}`,
          );
        }
      }
    }
  }

  /**
   * Check if a specific port exists in the service's port mappings
   */
  private hasPort(targetPort: number): boolean {
    for (const portMapping of this.ports) {
      const extractedPort = this.extractContainerPort(portMapping);
      if (extractedPort === targetPort) {
        return true;
      }
    }
    return false;
  }

  /**
   * Extract container port from port mapping
   */
  private extractContainerPort(portMapping: string): number {
    // Remove protocol suffix if present
    const portWithoutProtocol = portMapping.replace(/(\/tcp|\/udp)$/, "");
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
      return 0; // Invalid format
    }

    const port = parseInt(containerPortStr, 10);
    return isNaN(port) ? 0 : port;
  }

  /**
   * Validates port mapping format
   */
  private isValidPortMapping(port: string): boolean {
    const portWithoutProtocol = port.replace(/(\/tcp|\/udp)$/, "");
    const parts = portWithoutProtocol.split(":");

    if (parts.length === 1) {
      const containerPort = parseInt(parts[0], 10);
      return !isNaN(containerPort) && containerPort > 0 &&
        containerPort <= 65535;
    } else if (parts.length === 2) {
      const hostPort = parseInt(parts[0], 10);
      const containerPort = parseInt(parts[1], 10);
      return !isNaN(hostPort) && !isNaN(containerPort) &&
        hostPort > 0 && hostPort <= 65535 &&
        containerPort > 0 && containerPort <= 65535;
    } else if (parts.length === 3) {
      const hostIp = parts[0];
      const hostPort = parseInt(parts[1], 10);
      const containerPort = parseInt(parts[2], 10);

      const ipRegex = /^(\d{1,3}\.){3}\d{1,3}$/;
      const isValidIp = ipRegex.test(hostIp) &&
        hostIp.split(".").every((octet) => {
          const num = parseInt(octet, 10);
          return num >= 0 && num <= 255;
        });

      return isValidIp &&
        !isNaN(hostPort) && !isNaN(containerPort) &&
        hostPort > 0 && hostPort <= 65535 &&
        containerPort > 0 && containerPort <= 65535;
    }

    return false;
  }

  /**
   * Validates volume mapping format
   */
  private isValidVolumeMapping(volume: string): boolean {
    const parts = volume.split(":");
    return parts.length >= 2 && parts.length <= 3;
  }

  /**
   * Validates mounts (files or directories)
   */
  private validateMounts(
    mounts: MountConfig[],
    type: "file" | "directory",
  ): void {
    for (const mount of mounts) {
      if (!this.isValidMountConfig(mount, type)) {
        const mountStr = typeof mount === "string"
          ? mount
          : JSON.stringify(mount);
        throw new ConfigurationError(
          `Invalid ${type} mount '${mountStr}' for service '${this.name}'. Expected format: local:remote[:options] or { local, remote, mode?, owner?, options? }`,
        );
      }
    }
  }

  /**
   * Validates file or directory mount configuration
   */
  private isValidMountConfig(
    mount: FileMountConfig | DirectoryMountConfig,
    _type: "file" | "directory",
  ): boolean {
    if (typeof mount === "string") {
      const parts = mount.split(":");
      if (parts.length < 2 || parts.length > 3) {
        return false;
      }
      if (!parts[0] || !parts[1]) {
        return false;
      }
      if (parts.length === 3 && parts[2]) {
        const validOptions = ["ro", "z", "Z"];
        return validOptions.includes(parts[2]);
      }
      return true;
    } else if (typeof mount === "object" && mount !== null) {
      if (!mount.local || !mount.remote) {
        return false;
      }
      if (mount.mode && !/^[0-7]{3,4}$/.test(mount.mode)) {
        return false;
      }
      if (mount.owner && !/^[a-zA-Z0-9_-]+:[a-zA-Z0-9_-]+$/.test(mount.owner)) {
        return false;
      }
      if (mount.options) {
        const validOptions = ["ro", "z", "Z"];
        const options = mount.options.split(",").map((o) => o.trim());
        return options.every((opt) => validOptions.includes(opt));
      }
      return true;
    }
    return false;
  }

  /**
   * Returns the service configuration as a plain object
   */
  toObject(): Record<string, unknown> {
    const result: Record<string, unknown> = {};

    if (this.image) {
      result.image = this.image;
    }

    if (this.build) {
      result.build = this.build;
    }

    // Filter out undefined values from servers
    result.servers = this.servers.map((server) => {
      const cleanServer: Record<string, string> = {
        host: server.host,
      };
      if (server.arch !== undefined) {
        cleanServer.arch = server.arch;
      }
      if (server.alias !== undefined) {
        cleanServer.alias = server.alias;
      }
      return cleanServer;
    });

    if (this.ports.length > 0) {
      result.ports = this.ports;
    }

    if (this.volumes.length > 0) {
      result.volumes = this.volumes;
    }

    if (this.files.length > 0) {
      result.files = this.files;
    }

    if (this.directories.length > 0) {
      result.directories = this.directories;
    }

    const envObj = this.environment.toObject();
    if (Object.keys(envObj).length > 0) {
      result.environment = envObj;
    }

    if (this.command) {
      result.command = this.command;
    }

    return result;
  }

  /**
   * Returns true if this service should be built (has build config)
   */
  requiresBuild(): boolean {
    return !!this.build;
  }

  /**
   * Returns the image name to use (either from image or generated from build)
   * @param registry Optional registry prefix (e.g., "localhost:6767")
   * @param version Optional version tag (e.g., "a1b2c3d" or "latest")
   */
  getImageName(registry?: string, version?: string): string {
    if (this.image) {
      if (version) {
        const imageParts = this.image.split(":");
        if (imageParts.length === 2) {
          return `${imageParts[0]}:${version}`;
        } else {
          return `${this.image}:${version}`;
        }
      }

      return this.image;
    }
    const imageName = `${this.project}-${this.name}`;
    const imageTag = version || "latest";
    const fullName = `${imageName}:${imageTag}`;

    return registry ? `${registry}/${fullName}` : fullName;
  }

  /**
   * Generate container name using project and service name
   */
  getContainerName(suffix?: string): string {
    const baseName = `${this.project}-${this.name}`;
    return suffix ? `${baseName}-${suffix}` : baseName;
  }

  /**
   * Extracts named volumes from service's volume configuration.
   * Named volumes are those that don't start with "/" or "./" (not host paths).
   *
   * @returns Array of named volume names
   *
   * @example
   * volumes: ["db-data:/var/lib/postgresql/data", "./config:/etc/app"]
   * Returns: ["db-data"]
   */
  getNamedVolumes(): string[] {
    const namedVolumes: string[] = [];

    for (const volume of this.volumes) {
      const parts = volume.split(":");
      if (parts.length >= 2) {
        const source = parts[0];
        // Named volumes don't start with "/" or "./" (host paths do)
        if (!source.startsWith("/") && !source.startsWith("./")) {
          namedVolumes.push(source);
        }
      }
    }

    return namedVolumes;
  }

  /**
   * Validate architecture values
   */
  private validateArch(arch: unknown): string | string[] {
    const validArchs = ["amd64", "arm64"];

    if (typeof arch === "string") {
      if (!validArchs.includes(arch)) {
        throw new ConfigurationError(
          `Invalid architecture '${arch}' for service '${this.name}'. Allowed values: ${
            validArchs.join(", ")
          }`,
        );
      }
      return arch;
    }

    if (Array.isArray(arch)) {
      if (arch.length === 0) {
        throw new ConfigurationError(
          `Architecture array cannot be empty for service '${this.name}'`,
        );
      }

      for (const a of arch) {
        if (typeof a !== "string") {
          throw new ConfigurationError(
            `Architecture values must be strings for service '${this.name}'`,
          );
        }
        if (!validArchs.includes(a)) {
          throw new ConfigurationError(
            `Invalid architecture '${a}' for service '${this.name}'. Allowed values: ${
              validArchs.join(", ")
            }`,
          );
        }
      }

      return [...new Set(arch)];
    }

    throw new ConfigurationError(
      `Architecture must be a string or array of strings for service '${this.name}'`,
    );
  }

  /**
   * Get the default architecture for builds
   */
  static getDefaultArch(): string {
    return "amd64";
  }

  /**
   * Validate servers configuration
   */
  private validateServers(servers: unknown): ServerConfig[] {
    if (!Array.isArray(servers)) {
      throw new ConfigurationError(
        `'servers' for service '${this.name}' must be an array`,
      );
    }

    return servers.map((server, index) => {
      if (typeof server === "object" && server !== null) {
        const serverObj = server as Record<string, unknown>;
        if (!serverObj.host || typeof serverObj.host !== "string") {
          throw new ConfigurationError(
            `Server at index ${index} for service '${this.name}' must have a 'host' property`,
          );
        }
        if (serverObj.arch && typeof serverObj.arch !== "string") {
          throw new ConfigurationError(
            `Server architecture at index ${index} for service '${this.name}' must be a string`,
          );
        }
        if (serverObj.alias !== undefined) {
          if (typeof serverObj.alias !== "string") {
            throw new ConfigurationError(
              `Server alias at index ${index} for service '${this.name}' must be a string`,
            );
          }
          // Validate alias is DNS-safe: alphanumeric and hyphens only, no leading/trailing hyphens
          const dnsPattern = /^[a-zA-Z0-9]([a-zA-Z0-9-]*[a-zA-Z0-9])?$/;
          if (!dnsPattern.test(serverObj.alias)) {
            throw new ConfigurationError(
              `Server alias at index ${index} for service '${this.name}' must contain only alphanumeric characters and hyphens, and cannot start or end with a hyphen`,
            );
          }
        }

        // Build server config object, only including defined fields
        const serverConfig: ServerConfig = {
          host: serverObj.host,
        };
        if (serverObj.arch !== undefined) {
          serverConfig.arch = serverObj.arch as string;
        }
        if (serverObj.alias !== undefined) {
          serverConfig.alias = serverObj.alias as string;
        }
        return serverConfig;
      } else {
        throw new ConfigurationError(
          `Server at index ${index} for service '${this.name}' must be an object with 'host' property`,
        );
      }
    });
  }

  /**
   * Get all unique architectures required by this service's servers
   */
  getRequiredArchitectures(): string[] {
    const architectures = new Set<string>();

    const defaultArch = ServiceConfiguration.getDefaultArch();

    for (const serverConfig of this.servers) {
      const arch = serverConfig.arch || defaultArch;
      architectures.add(arch);
    }

    return Array.from(architectures);
  }

  /**
   * Get servers by architecture
   */
  getServersByArchitecture(): Map<string, string[]> {
    const serversByArch = new Map<string, string[]>();

    const defaultArch = ServiceConfiguration.getDefaultArch();

    for (const serverConfig of this.servers) {
      const serverAddress = serverConfig.host;
      const arch = serverConfig.arch || defaultArch;

      if (!serversByArch.has(arch)) {
        serversByArch.set(arch, []);
      }
      serversByArch.get(arch)!.push(serverAddress);
    }

    return serversByArch;
  }
}
