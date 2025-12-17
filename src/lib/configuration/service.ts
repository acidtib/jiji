import {
  BaseConfiguration,
  ConfigurationError,
  type Validatable,
} from "./base.ts";
import { ProxyConfiguration } from "./proxy.ts";

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
 * Mount options for files and directories
 */
type MountOptions = "ro" | "z" | "Z";

/**
 * File mount configuration - supports both string and hash formats
 * String format: "local:remote" or "local:remote:options"
 * Hash format: { local: string, remote: string, mode?: string, owner?: string, options?: string }
 */
export type FileMountConfig = string | {
  local: string;
  remote: string;
  mode?: string;
  owner?: string;
  options?: MountOptions | string;
};

/**
 * Directory mount configuration - supports both string and hash formats
 * String format: "local:remote" or "local:remote:options"
 * Hash format: { local: string, remote: string, mode?: string, owner?: string, options?: string }
 */
export type DirectoryMountConfig = string | {
  local: string;
  remote: string;
  mode?: string;
  owner?: string;
  options?: MountOptions | string;
};

/**
 * Service configuration representing a single deployable service
 */
export class ServiceConfiguration extends BaseConfiguration
  implements Validatable {
  private _name: string;
  private _project: string;
  private _image?: string;
  private _build?: string | BuildConfig;
  private _hosts?: string[];
  private _ports?: string[];
  private _volumes?: string[];
  private _files?: FileMountConfig[];
  private _directories?: DirectoryMountConfig[];
  private _environment?: Record<string, string> | string[];
  private _command?: string | string[];
  private _proxy?: ProxyConfiguration;

  constructor(name: string, config: Record<string, unknown>, project: string) {
    super(config);
    this._name = name;
    this._project = project;
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
   * List of hosts to deploy to
   */
  get hosts(): string[] {
    if (!this._hosts) {
      this._hosts = this.has("hosts")
        ? this.validateArray<string>(this.get("hosts"), "hosts", this.name)
        : [];
    }
    return this._hosts;
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
   * Environment variables
   */
  get environment(): Record<string, string> | string[] {
    if (!this._environment && this.has("environment")) {
      const envValue = this.get("environment");
      if (Array.isArray(envValue)) {
        this._environment = envValue as string[];
      } else if (typeof envValue === "object" && envValue !== null) {
        this._environment = envValue as Record<string, string>;
      } else {
        throw new ConfigurationError(
          `'environment' for service '${this.name}' must be an array or object`,
        );
      }
    }
    return this._environment || {};
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
          `Invalid port mapping '${port}' for service '${this.name}'. Expected format: [host_ip:]host_port:container_port[/protocol]`,
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

    // Validate file mounts
    for (const file of this.files) {
      if (!this.isValidMountConfig(file, "file")) {
        const fileStr = typeof file === "string" ? file : JSON.stringify(file);
        throw new ConfigurationError(
          `Invalid file mount '${fileStr}' for service '${this.name}'. Expected format: local:remote[:options] or { local, remote, mode?, owner?, options? }`,
        );
      }
    }

    // Validate directory mounts
    for (const directory of this.directories) {
      if (!this.isValidMountConfig(directory, "directory")) {
        const dirStr = typeof directory === "string"
          ? directory
          : JSON.stringify(directory);
        throw new ConfigurationError(
          `Invalid directory mount '${dirStr}' for service '${this.name}'. Expected format: local:remote[:options] or { local, remote, mode?, owner?, options? }`,
        );
      }
    }

    // Validate environment variables
    if (Array.isArray(this.environment)) {
      for (const env of this.environment) {
        if (!env.includes("=")) {
          throw new ConfigurationError(
            `Invalid environment variable '${env}' for service '${this.name}'. Expected format: KEY=value`,
          );
        }
      }
    }

    // Validate hosts are not empty
    if (this.hosts.length === 0) {
      throw new ConfigurationError(
        `Service '${this.name}' must specify at least one host`,
      );
    }

    // Validate each host
    for (const host of this.hosts) {
      this.validateHost(host, "hosts", this.name);
    }

    // Validate proxy configuration if present
    if (this.proxy) {
      this.proxy.validate();
    }
  }

  /**
   * Validates port mapping format
   */
  private isValidPortMapping(port: string): boolean {
    // Basic validation for port mapping format
    // Examples: "80:80", "127.0.0.1:80:80", "3000:3000/tcp"
    const portRegex =
      /^(\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}:)?\d+:\d+(\/tcp|\/udp)?$/;
    return portRegex.test(port);
  }

  /**
   * Validates volume mapping format
   */
  private isValidVolumeMapping(volume: string): boolean {
    // Basic validation for volume mapping format
    // Examples: "/host/path:/container/path", "/host/path:/container/path:ro"
    const parts = volume.split(":");
    return parts.length >= 2 && parts.length <= 3;
  }

  /**
   * Validates file or directory mount configuration
   */
  private isValidMountConfig(
    mount: FileMountConfig | DirectoryMountConfig,
    _type: "file" | "directory",
  ): boolean {
    if (typeof mount === "string") {
      // String format: "local:remote" or "local:remote:options"
      const parts = mount.split(":");
      if (parts.length < 2 || parts.length > 3) {
        return false;
      }
      // Check that local and remote are not empty
      if (!parts[0] || !parts[1]) {
        return false;
      }
      // If options are provided, validate they're valid
      if (parts.length === 3 && parts[2]) {
        const validOptions = ["ro", "z", "Z"];
        return validOptions.includes(parts[2]);
      }
      return true;
    } else if (typeof mount === "object" && mount !== null) {
      // Hash format validation
      if (!mount.local || !mount.remote) {
        return false;
      }
      // Validate mode format if provided (should be octal like "0644")
      if (mount.mode && !/^[0-7]{3,4}$/.test(mount.mode)) {
        return false;
      }
      // Validate owner format if provided (should be "user:group" or "uid:gid")
      if (mount.owner && !/^[a-zA-Z0-9_-]+:[a-zA-Z0-9_-]+$/.test(mount.owner)) {
        return false;
      }
      // Validate options if provided
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

    result.hosts = this.hosts;

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

    if (Object.keys(this.environment).length > 0) {
      result.environment = this.environment;
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
   */
  getImageName(registry?: string): string {
    if (this.image) {
      return this.image;
    }

    // Generate image name from project and service name
    const imageName = `${this.project}-${this.name}:latest`;
    return registry ? `${registry}/${imageName}` : imageName;
  }

  /**
   * Generate container name using project and service name
   */
  getContainerName(suffix?: string): string {
    const baseName = `${this.project}-${this.name}`;
    return suffix ? `${baseName}-${suffix}` : baseName;
  }
}
