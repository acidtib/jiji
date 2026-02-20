/**
 * Service for managing registry authentication both locally and on remote servers
 */

import type { ContainerEngine } from "../configuration/builder.ts";
import type { RegistryConfiguration } from "../configuration/registry.ts";
import type { SSHManager } from "../../utils/ssh.ts";
import { RegistryAuthenticator } from "../registry_authenticator.ts";
import { log } from "../../utils/logger.ts";
import type { RemoteAuthResult } from "../../types.ts";

/**
 * Service for handling registry authentication across local and remote hosts
 */
export class RegistryAuthService {
  private localAuthenticator: RegistryAuthenticator;
  private isLocallyAuthenticated = false;

  constructor(
    private engine: ContainerEngine,
    private registry: RegistryConfiguration,
  ) {
    this.localAuthenticator = new RegistryAuthenticator(engine);
  }

  /**
   * Authenticate locally (for building images)
   *
   * @returns True if authenticated successfully or already authenticated
   */
  async authenticateLocally(): Promise<boolean> {
    // Skip if already authenticated or using local registry
    if (this.isLocallyAuthenticated || this.registry.isLocal()) {
      return true;
    }

    const registryUrl = this.registry.getRegistryUrl();
    const username = this.registry.username;
    const password = this.registry.password;

    if (!username || !password) {
      log.warn(
        `No credentials configured for ${registryUrl}, skipping authentication`,
        "registry",
      );
      return false;
    }

    try {
      log.status(`Authenticating to ${registryUrl}...`, "registry");
      await this.localAuthenticator.login(registryUrl, {
        username,
        password,
      });
      this.isLocallyAuthenticated = true;
      log.success(`Authenticated to ${registryUrl}`, "registry");
      return true;
    } catch (error) {
      log.error(
        `Failed to authenticate to ${registryUrl}: ${
          error instanceof Error ? error.message : String(error)
        }`,
        "registry",
      );
      throw error;
    }
  }

  /**
   * Authenticate on a remote server (for pulling images)
   *
   * @param ssh SSH manager for the remote host
   * @param envVars Pre-loaded environment variables for secret resolution
   * @param allowHostEnv Whether to allow host environment fallback
   * @returns True if authenticated successfully
   */
  async authenticateRemotely(
    ssh: SSHManager,
    envVars: Record<string, string> = {},
    allowHostEnv: boolean = false,
  ): Promise<boolean> {
    // Skip authentication for local registries
    if (this.registry.isLocal()) {
      log.debug(
        `Skipping remote authentication for local registry on ${ssh.getHost()}`,
        "registry",
      );
      return true;
    }

    const host = ssh.getHost();
    const registryUrl = this.registry.getRegistryUrl();
    const username = this.registry.username;
    const password = this.registry.resolvePassword(envVars, allowHostEnv);

    if (!username || !password) {
      log.warn(
        `No credentials for ${registryUrl} on ${host}, skipping authentication`,
        "registry",
      );
      return false;
    }

    try {
      log.status(`Authenticating to ${registryUrl} on ${host}`, "registry");

      // Pipe password via stdin to avoid embedding secrets in shell strings
      const loginCommand =
        `${this.engine} login ${registryUrl} --username ${username} --password-stdin`;
      const loginResult = await ssh.executeCommandWithInput(
        loginCommand,
        password,
      );

      if (!loginResult.success) {
        throw new Error(loginResult.stderr || "Login failed");
      }

      log.success(`Authenticated to ${registryUrl} on ${host}`, "registry");
      return true;
    } catch (error) {
      const errorMessage = error instanceof Error
        ? error.message
        : String(error);
      log.warn(
        `Failed to authenticate to ${registryUrl} on ${host}: ${errorMessage}`,
        "registry",
      );
      return false;
    }
  }

  /**
   * Authenticate on multiple remote servers
   *
   * @param sshManagers SSH managers for remote hosts
   * @returns Array of authentication results
   */
  async authenticateRemoteHosts(
    sshManagers: SSHManager[],
  ): Promise<RemoteAuthResult[]> {
    // Skip for local registries
    if (this.registry.isLocal()) {
      return sshManagers.map((ssh) => ({
        host: ssh.getHost(),
        success: true,
      }));
    }

    const results: RemoteAuthResult[] = [];

    for (const ssh of sshManagers) {
      const host = ssh.getHost();
      try {
        const success = await this.authenticateRemotely(ssh);
        results.push({ host, success });
      } catch (error) {
        results.push({
          host,
          success: false,
          error: error instanceof Error ? error.message : String(error),
        });
      }
    }

    // Log summary
    const successCount = results.filter((r) => r.success).length;
    const failCount = results.filter((r) => !r.success).length;

    if (successCount > 0) {
      log.success(
        `Registry authenticated on ${successCount} host(s)`,
        "registry",
      );
    }
    if (failCount > 0) {
      log.warn(
        `Registry authentication failed on ${failCount} host(s)`,
        "registry",
      );
    }

    return results;
  }

  /**
   * Check if local authentication is required
   *
   * @returns True if authentication is needed
   */
  requiresLocalAuth(): boolean {
    return !this.registry.isLocal() &&
      !!this.registry.username &&
      !!this.registry.password;
  }

  /**
   * Check if remote authentication is required
   *
   * @returns True if authentication is needed
   */
  requiresRemoteAuth(): boolean {
    return !this.registry.isLocal() &&
      !!this.registry.username &&
      !!this.registry.password;
  }

  /**
   * Get registry URL
   *
   * @returns Registry URL
   */
  getRegistryUrl(): string {
    return this.registry.getRegistryUrl();
  }

  /**
   * Check if using local registry
   *
   * @returns True if local registry
   */
  isLocalRegistry(): boolean {
    return this.registry.isLocal();
  }
}
