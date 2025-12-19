import { log } from "../utils/logger.ts";
import type { ContainerEngine } from "./configuration/builder.ts";
import {
  createRegistryError,
  getErrorMessage,
  type RegistryErrorCode,
  RegistryErrorCodes,
} from "../utils/error_handling.ts";

/**
 * Registry credentials interface
 */
export interface RegistryCredentials {
  username: string;
  password: string;
}

/**
 * Authentication result interface
 */
export interface AuthenticationResult {
  success: boolean;
  registry: string;
  message?: string;
}

/**
 * Registry authenticator for handling container engine authentication
 */
export class RegistryAuthenticator {
  constructor(private engine: ContainerEngine) {}

  /**
   * Login to a registry using container engine
   */
  async login(
    registry: string,
    credentials?: RegistryCredentials,
  ): Promise<AuthenticationResult> {
    log.debug(
      `Attempting login to registry: ${registry}`,
      "registry:auth",
    );

    try {
      // Skip authentication for local registries without credentials
      if (this.isLocalRegistry(registry) && !credentials) {
        log.debug("Local registry, no authentication needed", "registry:auth");
        return {
          success: true,
          registry,
          message: "Local registry authentication skipped",
        };
      }

      await this.executeLogin(registry, credentials);

      log.debug(
        `Successfully authenticated to registry: ${registry}`,
        "registry:auth",
      );

      return {
        success: true,
        registry,
        message: "Authentication successful",
      };
    } catch (error) {
      const message = getErrorMessage(error);
      log.error(
        `Failed to authenticate to registry ${registry}: ${message}`,
        "registry:auth",
      );

      throw createRegistryError(
        `Authentication failed: ${message}`,
        this.mapAuthErrorCode(error),
        registry,
        "login",
        error instanceof Error ? error : undefined,
      );
    }
  }

  /**
   * Logout from a registry using container engine
   */
  async logout(registry: string): Promise<AuthenticationResult> {
    log.debug(`Attempting logout from registry: ${registry}`, "registry:auth");

    try {
      await this.executeLogout(registry);

      log.debug(
        `Successfully logged out from registry: ${registry}`,
        "registry:auth",
      );

      return {
        success: true,
        registry,
        message: "Logout successful",
      };
    } catch (error) {
      const message = getErrorMessage(error);

      // If already not logged in, treat as success
      if (message.toLowerCase().includes("not logged in")) {
        log.debug(
          `Already not logged into registry: ${registry}`,
          "registry:auth",
        );
        return {
          success: true,
          registry,
          message: "Already not logged in",
        };
      }

      log.error(
        `Failed to logout from registry ${registry}: ${message}`,
        "registry:auth",
      );

      throw createRegistryError(
        `Logout failed: ${message}`,
        RegistryErrorCodes.OPERATION_FAILED,
        registry,
        "logout",
        error instanceof Error ? error : undefined,
      );
    }
  }

  /**
   * Check if the current user is authenticated to a registry
   */
  async isAuthenticated(_registry: string): Promise<boolean> {
    try {
      // Try a lightweight operation to check authentication
      const command = new Deno.Command(this.engine, {
        args: ["system", "info"],
        stdout: "piped",
        stderr: "piped",
      });

      const { code } = await command.output();
      return code === 0;
    } catch {
      return false;
    }
  }

  /**
   * Execute login command with container engine
   */
  private async executeLogin(
    registry: string,
    credentials?: RegistryCredentials,
  ): Promise<void> {
    const loginArgs = ["login"];

    if (credentials?.username) {
      loginArgs.push("--username", credentials.username);
    }

    if (credentials?.password) {
      loginArgs.push("--password-stdin");
    }

    loginArgs.push(registry);

    const command = new Deno.Command(this.engine, {
      args: loginArgs,
      stdin: credentials?.password ? "piped" : "null",
      stdout: "piped",
      stderr: "piped",
    });

    const process = command.spawn();

    // Send password via stdin if provided
    if (credentials?.password) {
      const writer = process.stdin.getWriter();
      try {
        await writer.write(new TextEncoder().encode(credentials.password));
        await writer.close();
      } catch (error) {
        log.error(
          `Failed to send password to ${this.engine}: ${
            getErrorMessage(error)
          }`,
          "registry:auth",
        );
        throw error;
      }
    }

    const { code, stderr } = await process.output();

    if (code !== 0) {
      const error = new TextDecoder().decode(stderr);
      throw new Error(`${this.engine} login failed: ${error}`);
    }
  }

  /**
   * Execute logout command with container engine
   */
  private async executeLogout(registry: string): Promise<void> {
    const command = new Deno.Command(this.engine, {
      args: ["logout", registry],
      stdout: "piped",
      stderr: "piped",
    });

    const { code, stderr } = await command.output();

    if (code !== 0) {
      const error = new TextDecoder().decode(stderr);
      throw new Error(`${this.engine} logout failed: ${error}`);
    }
  }

  /**
   * Check if a registry URL is a local registry
   */
  private isLocalRegistry(registry: string): boolean {
    return registry.includes("localhost") ||
      registry.includes("127.0.0.1") ||
      registry.includes("0.0.0.0");
  }

  /**
   * Map authentication errors to specific error codes
   */
  private mapAuthErrorCode(error: unknown): RegistryErrorCode {
    if (!(error instanceof Error)) {
      return RegistryErrorCodes.UNKNOWN_ERROR;
    }

    const message = error.message.toLowerCase();

    if (message.includes("unauthorized") || message.includes("denied")) {
      return RegistryErrorCodes.INVALID_CREDENTIALS;
    }

    if (message.includes("not found") || message.includes("no such")) {
      return RegistryErrorCodes.ENGINE_NOT_FOUND;
    }

    if (
      message.includes("connection refused") ||
      message.includes("unreachable")
    ) {
      return RegistryErrorCodes.REGISTRY_UNREACHABLE;
    }

    if (message.includes("timeout")) {
      return RegistryErrorCodes.CONNECTION_TIMEOUT;
    }

    return RegistryErrorCodes.AUTH_FAILED;
  }

  /**
   * Get container engine name
   */
  getEngine(): ContainerEngine {
    return this.engine;
  }
}
