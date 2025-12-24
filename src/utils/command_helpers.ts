/**
 * Command helper utilities for common command patterns
 */

import type { GlobalOptions } from "../types.ts";
import { Configuration } from "../lib/configuration.ts";
import { setupSSHConnections, type SSHManager } from "./ssh.ts";
import { log } from "./logger.ts";

/**
 * Execute a command with best-effort semantics (ignore failures)
 *
 * Use this for cleanup operations where failures should be logged but not block execution.
 * Replaces the pattern: `command 2>/dev/null || true`
 *
 * @param ssh SSH manager to execute command on
 * @param command Command to execute
 * @param context Optional context for logging (e.g., "removing container", "stopping service")
 * @returns Promise that resolves when command completes (success or failure)
 *
 * @example
 * ```typescript
 * await executeBestEffort(ssh, "docker rm -f my-container", "removing container");
 * ```
 */
export async function executeBestEffort(
  ssh: SSHManager,
  command: string,
  context?: string,
): Promise<void> {
  const result = await ssh.executeCommand(command);

  if (!result.success && context) {
    log.debug(
      `Best-effort command failed (${context}): ${
        result.stderr || result.stdout
      }`,
    );
  }
}

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
  const config = await Configuration.load(
    globalOptions.environment,
    globalOptions.configFile,
  );
  const configPath = config.configPath || "unknown";
  let allHosts = config.getAllServerHosts();
  let matchingServices: string[] | undefined;

  if (globalOptions.services && !options.skipServiceFiltering) {
    const requestedServices = globalOptions.services.split(",").map((s) =>
      s.trim()
    );

    matchingServices = config.getMatchingServiceNames(requestedServices);

    if (matchingServices.length === 0) {
      log.error(
        `No services found matching: ${requestedServices.join(", ")}`,
      );
      log.say(
        `Available services: ${config.getServiceNames().join(", ")}`,
        1,
      );
      Deno.exit(1);
    }

    allHosts = config.getHostsFromServices(matchingServices);

    log.say(`Targeting services: ${matchingServices.join(", ")}`, 1);
    log.say(`Service hosts: ${allHosts.join(", ")}`, 1);
  }

  if (globalOptions.hosts && !options.skipHostFiltering) {
    const requestedHosts = globalOptions.hosts.split(",").map((h) => h.trim());
    const validHosts = requestedHosts.filter((host) => allHosts.includes(host));
    const invalidHosts = requestedHosts.filter((host) =>
      !allHosts.includes(host)
    );

    if (invalidHosts.length > 0) {
      log.warn(
        `Invalid hosts specified (not in config): ${invalidHosts.join(", ")}`,
      );
    }

    if (validHosts.length === 0) {
      log.error("No valid hosts specified");
      Deno.exit(1);
    }

    allHosts = validHosts;
    log.say(`Targeting specific hosts: ${allHosts.join(", ")}`, 1);
  }

  if (allHosts.length === 0) {
    log.error(
      `No remote hosts found in configuration at: ${configPath}`,
    );
    log.say(
      `Could not find any hosts. Please update your jiji config to include hosts for services.`,
      1,
    );
    Deno.exit(1);
  }

  const sshTracker = log.createStepTracker("SSH Connection Setup:");
  const sshResult = await setupSSHConnections(
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
  sshTracker.finish();

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
      log.debug(`Failed to dispose SSH connection: ${error}`);
    }
  });
}

/**
 * Find an SSH manager by host name
 *
 * This helper consolidates the common pattern of searching for an SSH manager
 * by hostname across the codebase (found in 15+ locations).
 *
 * @param sshManagers Array of SSH managers
 * @param host Host name to search for
 * @returns SSH manager for the host, or undefined if not found
 *
 * @example
 * ```typescript
 * const ssh = findSSHManagerByHost(sshManagers, "server1.example.com");
 * if (ssh) {
 *   await ssh.execute("docker ps");
 * }
 * ```
 */
export function findSSHManagerByHost(
  sshManagers: SSHManager[],
  host: string,
): SSHManager | undefined {
  return sshManagers.find((ssh) => ssh.getHost() === host);
}

/**
 * Display standardized command header with configuration details and connection status
 *
 * This consolidates the common pattern used across commands for showing:
 * - Section title
 * - Configuration file path
 * - Container engine
 * - Remote hosts found
 * - SSH connection status
 *
 * @param title Section title to display
 * @param config Loaded configuration
 * @param sshManagers SSH managers for connected hosts
 * @param options Additional display options
 *
 * @example
 * ```typescript
 * const ctx = await setupCommandContext(globalOptions);
 * displayCommandHeader("Deployment:", ctx.config, ctx.sshManagers);
 * ```
 */
export function displayCommandHeader(
  title: string,
  config: Configuration,
  sshManagers: SSHManager[],
  options: {
    showServices?: string[];
  } = {},
): void {
  log.section(title);

  const configPath = config.configPath || "unknown";
  const allHosts = config.getAllServerHosts();

  log.say(`Configuration loaded from: ${configPath}`, 1);
  log.say(`Container engine: ${config.builder.engine}`, 1);
  log.say(
    `Found ${allHosts.length} remote host(s): ${allHosts.join(", ")}`,
    1,
  );

  if (options.showServices && options.showServices.length > 0) {
    log.say(`Targeting services: ${options.showServices.join(", ")}`, 1);
  }

  console.log("");
  for (const ssh of sshManagers) {
    log.remote(ssh.getHost(), ": Connected", { indent: 1 });
  }
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
