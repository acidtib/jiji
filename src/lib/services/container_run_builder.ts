import type { ContainerEngine } from "../configuration/builder.ts";

/**
 * Fluent builder for constructing container run commands
 * Provides a clean, readable API for building complex run commands
 */
export class ContainerRunBuilder {
  private args: string[] = [];

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
   * Run in detached mode
   * @returns This builder for chaining
   */
  detached(): this {
    this.args.push("--detach");
    return this;
  }

  /**
   * Build the final command string
   * @returns Complete command string ready for execution
   */
  build(): string {
    // Add image name at the end
    const finalArgs = [...this.args, this.imageName];
    return `${this.engine} ${finalArgs.join(" ")}`;
  }

  /**
   * Build and return as argument array (useful for Deno.Command)
   * @returns Array of command arguments
   */
  buildArgs(): string[] {
    return [...this.args, this.imageName];
  }
}
