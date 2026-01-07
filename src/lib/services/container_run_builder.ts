import type { ContainerEngine } from "../configuration/builder.ts";

/**
 * Fluent builder for constructing container run commands
 * Provides a clean, readable API for building complex run commands
 */
export class ContainerRunBuilder {
  private args: string[] = [];
  private commandArgs: string[] = [];

  constructor(
    private engine: ContainerEngine,
    private containerName: string,
    private imageName: string,
  ) {
    this.args.push("run");
    this.args.push("--name", containerName);
  }

  /**
   * Add network configuration
   * @param networkName Network name to connect to
   * @returns This builder for chaining
   */
  network(networkName: string): this {
    this.args.push("--network", networkName);
    return this;
  }

  /**
   * Add DNS configuration
   * @param dnsServer DNS server IP address
   * @param searchDomain Optional DNS search domain
   * @returns This builder for chaining
   */
  dns(dnsServer: string, searchDomain?: string): this {
    this.args.push("--dns", dnsServer);
    if (searchDomain) {
      this.args.push("--dns-search", searchDomain);
      this.args.push("--dns-option", "ndots:1");
    }
    return this;
  }

  /**
   * Add port mappings
   * @param ports Array of port mappings (e.g., ["8080:80", "443:443"])
   * @returns This builder for chaining
   */
  ports(ports: string[]): this {
    for (const port of ports) {
      this.args.push("-p", port);
    }
    return this;
  }

  /**
   * Add volume mounts
   * @param mounts Array of volume mount specifications
   * @returns This builder for chaining
   */
  volumes(mounts: string): this {
    if (mounts.trim()) {
      // Split by space and add each mount
      const mountParts = mounts.trim().split(/\s+/);
      this.args.push(...mountParts);
    }
    return this;
  }

  /**
   * Add environment variables
   * @param envVars Array of environment variables in "KEY=VALUE" format
   * @returns This builder for chaining
   */
  environment(envVars: string[]): this {
    for (const envVar of envVars) {
      this.args.push("-e", envVar);
    }
    return this;
  }

  /**
   * Set restart policy
   * @param policy Restart policy (e.g., "unless-stopped", "always", "on-failure")
   * @returns This builder for chaining
   */
  restart(policy: string): this {
    this.args.push("--restart", policy);
    return this;
  }

  /**
   * Set CPU limit
   * @param cpus Number of CPUs (e.g., 0.5, 1, 2.5, or "1.5")
   * @returns This builder for chaining
   */
  cpus(cpus: number | string): this {
    this.args.push("--cpus", String(cpus));
    return this;
  }

  /**
   * Set memory limit
   * @param memory Memory limit (e.g., "512m", "1g", "2gb")
   * @returns This builder for chaining
   */
  memory(memory: string): this {
    this.args.push("--memory", memory);
    return this;
  }

  /**
   * Add GPU devices
   * @param gpus GPU specification (e.g., "all", "0", "0,1", "device=0")
   * @returns This builder for chaining
   */
  gpus(gpus: string): this {
    this.args.push("--gpus", gpus);
    return this;
  }

  /**
   * Add device mappings
   * @param devices Array of device paths (e.g., ["/dev/video0", "/dev/snd"])
   * @returns This builder for chaining
   */
  devices(devices: string[]): this {
    for (const device of devices) {
      this.args.push("--device", device);
    }
    return this;
  }

  /**
   * Run container in privileged mode
   * @returns This builder for chaining
   */
  privileged(): this {
    this.args.push("--privileged");
    return this;
  }

  /**
   * Add Linux capabilities to the container
   * @param capabilities Array of capabilities (e.g., ["SYS_ADMIN", "NET_ADMIN"])
   * @returns This builder for chaining
   */
  capAdd(capabilities: string[]): this {
    for (const cap of capabilities) {
      this.args.push("--cap-add", cap);
    }
    return this;
  }

  /**
   * Run in detached mode
   * @returns This builder for chaining
   */
  detached(): this {
    this.args.push("--detach");
    return this;
  }

  /**
   * Set container command (overrides image CMD)
   * @param cmd Command as string or array of arguments
   * @returns This builder for chaining
   */
  command(cmd: string | string[]): this {
    if (typeof cmd === "string") {
      // String format: pass as single argument (shell interpretation)
      this.commandArgs.push(cmd);
    } else if (Array.isArray(cmd)) {
      // Array format: each element is a separate argument
      this.commandArgs.push(...cmd);
    }
    return this;
  }

  /**
   * Build the final command string
   * @returns Complete command string ready for execution
   */
  build(): string {
    // Add image name, then command args at the end
    const finalArgs = [...this.args, this.imageName, ...this.commandArgs];
    // Properly escape arguments containing special shell characters
    const escapedArgs = finalArgs.map((arg) => this.escapeShellArg(arg));
    return `${this.engine} ${escapedArgs.join(" ")}`;
  }

  /**
   * Escape a shell argument if it contains special characters
   * Uses single quotes for safety with special chars like $, but handles embedded single quotes
   * @param arg The argument to escape
   * @returns The escaped argument
   */
  private escapeShellArg(arg: string): string {
    // If the argument doesn't contain special characters, return as-is
    if (!/[`$"\\!* (){}[\];'<>?&|~\n\t]/.test(arg)) {
      return arg;
    }

    // Use single quotes and escape any embedded single quotes
    // Single quotes preserve all special characters except single quote itself
    return `'${arg.replace(/'/g, "'\\''")}'`;
  }

  /**
   * Build and return as argument array (useful for Deno.Command)
   * @returns Array of command arguments
   */
  buildArgs(): string[] {
    return [...this.args, this.imageName, ...this.commandArgs];
  }
}
