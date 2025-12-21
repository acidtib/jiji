/**
 * Command helper utilities for common command patterns
 */

import type { GlobalOptions } from "../types.ts";
import { Configuration } from "../lib/configuration.ts";
import { setupSSHConnections, type SSHManager } from "./ssh.ts";
import { log } from "./logger.ts";

/**
 * Options for setting up command context
 */
export interface CommandContextOptions {
  /**
   * Allow partial connection (some hosts can fail to connect)
   */
  allowPartialConnection?: boolean;
  /**
   * Skip service filtering (use all hosts)
   */
  skipServiceFiltering?: boolean;
  /**
   * Skip host filtering (use all hosts)
   */
  skipHostFiltering?: boolean;
}

/**
 * Command context containing configuration, SSH managers, and target hosts
 */
export interface CommandContext {
  /**
   * Loaded configuration
   */
  config: Configuration;
  /**
   * SSH managers for connected hosts
   */
  sshManagers: SSHManager[];
  /**
   * Target hosts after filtering
   */
  targetHosts: string[];
  /**
   * Matching services if service filtering was applied
   */
  matchingServices?: string[];
}

/**
 * Set up command context with configuration loading, host filtering, and SSH connections
 *
 * This helper consolidates the common pattern used across most commands:
 * 1. Load configuration
 * 2. Collect and filter hosts based on global options
 * 3. Set up SSH connections
 *
 * @param globalOptions Global command options
 * @param options Additional options for context setup
 * @returns Command context with config, SSH managers, and target hosts
 *
 * @example
 * ```typescript
 * const ctx = await setupCommandContext(globalOptions);
 * try {
 *   // Use ctx.config, ctx.sshManagers, ctx.targetHosts
 * } finally {
 *   cleanupSSHConnections(ctx.sshManagers);
 * }
 * ```
 */
export async function setupCommandContext(
  globalOptions: GlobalOptions,
  options: CommandContextOptions = {},
): Promise<CommandContext> {
  // Load configuration
  const config = await Configuration.load(
    globalOptions.environment,
    globalOptions.configFile,
  );
  const configPath = config.configPath || "unknown";
  log.success(`Configuration loaded from: ${configPath}`, "config");

  // Collect all unique hosts from services
  let allHosts = config.getAllServerHosts();
  let matchingServices: string[] | undefined;

  // Filter by services if requested
  if (globalOptions.services && !options.skipServiceFiltering) {
    const requestedServices = globalOptions.services.split(",").map((s) =>
      s.trim()
    );

    // Get matching service names (supports wildcards)
    matchingServices = config.getMatchingServiceNames(requestedServices);

    if (matchingServices.length === 0) {
      log.error(
        `No services found matching: ${requestedServices.join(", ")}`,
        "filter",
      );
      log.info(
        `Available services: ${config.getServiceNames().join(", ")}`,
        "filter",
      );
      Deno.exit(1);
    }

    // Get hosts from matching services
    allHosts = config.getHostsFromServices(matchingServices);

    log.info(`Targeting services: ${matchingServices.join(", ")}`, "filter");
    log.info(`Service hosts: ${allHosts.join(", ")}`, "filter");
  }

  // Filter by hosts if requested
  if (globalOptions.hosts && !options.skipHostFiltering) {
    const requestedHosts = globalOptions.hosts.split(",").map((h) => h.trim());
    const validHosts = requestedHosts.filter((host) => allHosts.includes(host));
    const invalidHosts = requestedHosts.filter((host) =>
      !allHosts.includes(host)
    );

    if (invalidHosts.length > 0) {
      log.warn(
        `Invalid hosts specified (not in config): ${invalidHosts.join(", ")}`,
        "filter",
      );
    }

    if (validHosts.length === 0) {
      log.error("No valid hosts specified", "filter");
      Deno.exit(1);
    }

    allHosts = validHosts;
    log.info(`Targeting specific hosts: ${allHosts.join(", ")}`, "filter");
  }

  if (allHosts.length === 0) {
    log.error(
      `No remote hosts found in configuration at: ${configPath}`,
      "config",
    );
    log.error(
      `Could not find any hosts. Please update your jiji config to include hosts for services.`,
      "config",
    );
    Deno.exit(1);
  }

  log.info(
    `Found ${allHosts.length} remote host(s): ${allHosts.join(", ")}`,
    "ssh",
  );

  // Set up SSH connections
  let sshResult: Awaited<ReturnType<typeof setupSSHConnections>>;

  await log.group("SSH Connection Setup", async () => {
    sshResult = await setupSSHConnections(
      allHosts,
      {
        user: config.ssh.user,
        port: config.ssh.port,
        proxy: config.ssh.proxy,
        proxy_command: config.ssh.proxyCommand,
        keys: config.ssh.allKeys.length > 0 ? config.ssh.allKeys : undefined,
        keyData: config.ssh.keyData,
        keysOnly: config.ssh.keysOnly,
        dnsRetries: config.ssh.dnsRetries,
      },
      { allowPartialConnection: options.allowPartialConnection ?? true },
    );
  });

  return {
    config,
    sshManagers: sshResult!.managers,
    targetHosts: sshResult!.connectedHosts,
    matchingServices,
  };
}

/**
 * Clean up SSH connections
 *
 * @param sshManagers SSH managers to clean up
 */
export function cleanupSSHConnections(sshManagers: SSHManager[]): void {
  sshManagers.forEach((ssh) => {
    try {
      ssh.dispose();
    } catch (error) {
      // Ignore cleanup errors, but log them for debugging
      log.debug(`Failed to dispose SSH connection: ${error}`, "ssh");
    }
  });
}

/**
 * Resolve target hosts from configuration and global options
 * This is a lightweight version of setupCommandContext that doesn't establish SSH connections
 *
 * @param config Configuration object
 * @param globalOptions Global command options
 * @returns Object containing all hosts and optionally matching services
 */
export function resolveTargetHosts(
  config: Configuration,
  globalOptions: GlobalOptions,
): {
  allHosts: string[];
  matchingServices?: string[];
} {
  let allHosts = config.getAllServerHosts();
  let matchingServices: string[] | undefined;

  // Filter by services if requested
  if (globalOptions.services) {
    const requestedServices = globalOptions.services.split(",").map((s) =>
      s.trim()
    );
    matchingServices = config.getMatchingServiceNames(requestedServices);

    if (matchingServices.length === 0) {
      log.error(
        `No services found matching: ${requestedServices.join(", ")}`,
        "filter",
      );
      log.info(
        `Available services: ${config.getServiceNames().join(", ")}`,
        "filter",
      );
      Deno.exit(1);
    }

    allHosts = config.getHostsFromServices(matchingServices);
  }

  // Filter by hosts if requested
  if (globalOptions.hosts) {
    const requestedHosts = globalOptions.hosts.split(",").map((h) => h.trim());
    const validHosts = requestedHosts.filter((host) => allHosts.includes(host));

    if (validHosts.length === 0) {
      log.error("No valid hosts specified", "filter");
      Deno.exit(1);
    }

    allHosts = validHosts;
  }

  return { allHosts, matchingServices };
}

/**
 * Command handler function type
 */
export type CommandHandler<T = void> = (
  ctx: CommandContext,
) => Promise<T>;

/**
 * Wrapper for command execution with automatic context setup and cleanup
 *
 * This function provides a complete command execution pattern:
 * 1. Sets up command context (config, SSH, filtering)
 * 2. Executes the command handler
 * 3. Handles errors with audit logging
 * 4. Cleans up SSH connections
 *
 * @param globalOptions Global command options
 * @param operation Operation name (e.g., "Bootstrap", "Deployment")
 * @param component Component identifier for logging
 * @param handler Command handler function
 * @param contextOptions Options for context setup
 *
 * @example
 * ```typescript
 * export const myCommand = new Command()
 *   .description("My command")
 *   .action(async (options) => {
 *     await withCommandContext(
 *       options as unknown as GlobalOptions,
 *       "My Operation",
 *       "my-op",
 *       async (ctx) => {
 *         // Command logic here
 *         // Access ctx.config, ctx.sshManagers, ctx.targetHosts
 *       }
 *     );
 *   });
 * ```
 */
export async function withCommandContext<T = void>(
  globalOptions: GlobalOptions,
  operation: string,
  component: string,
  handler: CommandHandler<T>,
  contextOptions: CommandContextOptions = {},
): Promise<T> {
  let ctx: CommandContext | undefined;

  try {
    log.info(`Starting ${operation.toLowerCase()} process`, component);

    // Set up command context
    ctx = await setupCommandContext(globalOptions, contextOptions);

    // Execute the handler
    const result = await handler(ctx);

    log.success(`${operation} completed successfully`, component);
    return result;
  } catch (error) {
    const errorMessage = error instanceof Error ? error.message : String(error);
    log.error(`${operation} failed:`, component);
    log.error(errorMessage, component);

    // Log to audit trail if context was set up
    if (ctx?.sshManagers && ctx?.config && ctx?.targetHosts) {
      try {
        const { handleCommandError } = await import("./error_handler.ts");
        await handleCommandError(error, {
          operation,
          component,
          sshManagers: ctx.sshManagers,
          projectName: ctx.config.project,
          targetHosts: ctx.targetHosts,
        });
      } catch (auditError) {
        log.debug(
          `Failed to log to audit trail: ${auditError}`,
          component,
        );
      }
    }

    // TypeScript doesn't understand that Deno.exit never returns
    Deno.exit(1);
    // deno-lint-ignore no-unreachable
    throw new Error("Unreachable");
  } finally {
    // Always clean up SSH connections
    if (ctx?.sshManagers) {
      cleanupSSHConnections(ctx.sshManagers);
    }
  }
}
