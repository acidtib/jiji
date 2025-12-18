import { Command } from "@cliffy/command";
import { colors } from "@cliffy/ansi/colors";
import { filterHostsByPatterns, loadConfig } from "../utils/config.ts";
import type { GlobalOptions } from "../types.ts";
import type { Configuration } from "../lib/configuration.ts";
import { setupSSHConnections, type SSHManager } from "../utils/ssh.ts";
import { createServerAuditLogger } from "../utils/audit.ts";
import { log } from "../utils/logger.ts";

export const lockCommand = new Command()
  .description("Manage deployment locks");

// Subcommand: acquire
const acquireCommand = new Command()
  .description("Acquire a deployment lock to prevent concurrent deployments")
  .arguments("<message:string>")
  .option("--force", "Force acquire lock (use with caution)", {
    default: false,
  })
  .option("--timeout <seconds:number>", "Timeout in seconds", { default: 300 })
  .action(async (options, message) => {
    await acquireLock(message, options);
  });

// Subcommand: release
const releaseCommand = new Command()
  .description("Release the deployment lock")
  .action(async (_options) => {
    await releaseLock({});
  });

// Subcommand: status
const statusCommand = new Command()
  .description("Show lock status")
  .option("--json", "Output as JSON", { default: false })
  .action(async (options) => {
    await showLockStatus(options);
  });

// Subcommand: show
const showCommand = new Command()
  .description("Show detailed lock information")
  .action(async (_options) => {
    await showDetailedLockInfo({});
  });

// Add subcommands to main command
lockCommand
  .command("acquire", acquireCommand)
  .command("release", releaseCommand)
  .command("status", statusCommand)
  .command("show", showCommand);

interface LockInfo {
  locked: boolean;
  message?: string;
  acquiredAt?: string;
  acquiredBy?: string;
  host?: string;
  pid?: number;
}

/**
 * Acquire a deployment lock
 */
async function acquireLock(
  message: string,
  options: {
    config?: string;
    host?: string;
    force: boolean;
    timeout: number;
  },
): Promise<void> {
  let sshManagers: SSHManager[] | undefined;

  try {
    log.info(colors.bold("Acquiring deployment lock..."), "lock");

    // Cast options to GlobalOptions to access global options
    const globalOptions = options as unknown as GlobalOptions;
    const { config } = await loadConfig(globalOptions.configFile);
    const { targetHosts, sshManagers: managers } =
      await setupLockSSHConnections(
        config,
        globalOptions,
      );
    sshManagers = managers;

    const auditLogger = createServerAuditLogger(sshManagers!, config.project);

    // Check if any locks exist
    const lockStatuses = await checkLockStatus(sshManagers!);
    const activeLocks = lockStatuses.filter((status) =>
      status.locked && !options.force
    );

    if (activeLocks.length > 0) {
      log.error("Deployment lock already exists:", "lock");

      for (const lock of activeLocks) {
        log.error(
          `${colors.cyan(lock.host || "unknown")}: ${
            colors.yellow(lock.message || "No message")
          }`,
          "lock",
        );
        if (lock.acquiredBy) {
          log.error(`   Acquired by: ${colors.white(lock.acquiredBy)}`, "lock");
        }
        if (lock.acquiredAt) {
          log.error(
            `   Acquired at: ${
              colors.gray(new Date(lock.acquiredAt).toLocaleString())
            }`,
            "lock",
          );
        }
      }

      log.error(
        `Use ${colors.yellow("--force")} to override (use with caution)`,
        "lock",
      );
      Deno.exit(1);
    }

    // Acquire locks on all hosts
    log.info("Creating lock files...", "lock");

    const lockData: LockInfo = {
      locked: true,
      message,
      acquiredAt: new Date().toISOString(),
      acquiredBy: await getCurrentUser(),
      pid: Deno.pid,
    };

    const results = await Promise.all(
      sshManagers!.map(async (sshManager) => {
        const host = sshManager.getHost();
        try {
          const success = await createLockFile(sshManager, lockData);
          return { host, success, error: null };
        } catch (error) {
          return {
            host,
            success: false,
            error: error instanceof Error ? error.message : String(error),
          };
        }
      }),
    );

    const failures = results.filter((r) => !r.success);

    if (failures.length > 0) {
      log.error(
        "Failed to acquire locks on some hosts:",
        "lock",
      );
      for (const failure of failures) {
        log.error(`   ${colors.cyan(failure.host)}: ${failure.error}`, "lock");
      }

      // Try to clean up any successful locks
      log.warn("Cleaning up partial locks...", "lock");
      await cleanupPartialLocks(sshManagers);
      Deno.exit(1);
    }

    // Log successful lock acquisition
    await auditLogger.logEntry({
      action: "deployment_lock",
      status: "success",
      message: `Lock acquired: ${message}`,
      details: {
        message,
        hosts: targetHosts,
        acquiredBy: lockData.acquiredBy,
      },
    });

    log.success(
      "Deployment lock acquired successfully!",
      "lock",
    );
    log.info(`Message: ${colors.white(message)}`, "lock");
    log.info(`Hosts: ${colors.cyan(targetHosts.join(", "))}`, "lock");
    log.info(`To release: ${colors.yellow("jiji lock release")}`, "lock");
  } catch (error) {
    log.error(`Failed to acquire deployment lock:`, "lock");
    log.error(
      colors.red(error instanceof Error ? error.message : String(error)),
      "lock",
    );
    Deno.exit(1);
  } finally {
    cleanupSSHConnections(sshManagers);
  }
}

/**
 * Release a deployment lock
 */
async function releaseLock(
  options: {
    config?: string;
    host?: string;
  } = {},
): Promise<void> {
  let sshManagers: SSHManager[] | undefined;

  try {
    log.info(colors.bold("Releasing deployment lock..."), "lock");

    // Cast options to GlobalOptions to access global options
    const globalOptions = options as unknown as GlobalOptions;
    const { config } = await loadConfig(globalOptions.configFile);
    const { targetHosts: _targetHosts, sshManagers: managers } =
      await setupLockSSHConnections(
        config,
        globalOptions,
      );
    sshManagers = managers;

    const auditLogger = createServerAuditLogger(sshManagers!, config.project);

    // Check if locks exist
    const lockStatuses = await checkLockStatus(sshManagers!);
    const activeLocks = lockStatuses.filter((status) => status.locked);

    if (activeLocks.length === 0) {
      log.warn("No deployment locks found", "lock");
      return;
    }

    // Remove lock files
    log.info("Removing lock files...", "lock");

    const results = await Promise.all(
      sshManagers!.map(async (sshManager) => {
        const host = sshManager.getHost();
        try {
          const success = await removeLockFile(sshManager);
          return { host, success, error: null };
        } catch (error) {
          return {
            host,
            success: false,
            error: error instanceof Error ? error.message : String(error),
          };
        }
      }),
    );

    const failures = results.filter((r) => !r.success);

    if (failures.length > 0) {
      log.warn(
        "Failed to release locks on some hosts:",
        "lock",
      );
      for (const failure of failures) {
        log.error(`   ${colors.cyan(failure.host)}: ${failure.error}`, "lock");
      }
    }

    const successes = results.filter((r) => r.success);
    if (successes.length > 0) {
      await auditLogger.logEntry({
        action: "deployment_unlock",
        status: "success",
        message: `Lock released from ${successes.length} host(s)`,
        details: {
          hosts: successes.map((s) => s.host),
          releasedBy: await getCurrentUser(),
        },
      });

      log.success(
        "Deployment lock released successfully!",
        "lock",
      );
      log.info(
        `Hosts: ${colors.cyan(successes.map((s) => s.host).join(", "))}`,
        "lock",
      );
    }
  } catch (error) {
    log.error(`Failed to release deployment lock:`, "lock");
    log.error(
      colors.red(error instanceof Error ? error.message : String(error)),
      "lock",
    );
    Deno.exit(1);
  } finally {
    cleanupSSHConnections(sshManagers);
  }
}

/**
 * Show lock status for all hosts
 */
async function showLockStatus(
  options: {
    config?: string;
    host?: string;
    json: boolean;
  },
): Promise<void> {
  let sshManagers: SSHManager[] | undefined;

  try {
    // Cast options to GlobalOptions to access global options
    const globalOptions = options as unknown as GlobalOptions;
    const { config } = await loadConfig(globalOptions.configFile);
    const { targetHosts: _targetHosts, sshManagers: managers } =
      await setupLockSSHConnections(
        config,
        globalOptions,
      );
    sshManagers = managers;

    const lockStatuses = await checkLockStatus(sshManagers!);

    if (options.json) {
      log.info(
        JSON.stringify(
          {
            hosts: lockStatuses,
            summary: {
              total: lockStatuses.length,
              locked: lockStatuses.filter((s) => s.locked).length,
              unlocked: lockStatuses.filter((s) => !s.locked).length,
            },
          },
          null,
          2,
        ),
        "lock",
      );
      return;
    }

    log.info("Deployment Lock Status", "lock");

    const activeLocks = lockStatuses.filter((s) => s.locked);
    const unlockedHosts = lockStatuses.filter((s) => !s.locked);

    if (activeLocks.length === 0) {
      log.success("No active deployment locks", "lock");
    } else {
      log.error(`${activeLocks.length} active lock(s):`, "lock");
      for (const lock of activeLocks) {
        log.info(`\n${colors.cyan(lock.host || "unknown")}:`, "lock");
        log.error(`  Status: ${colors.red("LOCKED")}`, "lock");
        if (lock.message) {
          log.info(`  Message: ${colors.yellow(lock.message)}`, "lock");
        }
        if (lock.acquiredBy) {
          log.info(`  Owner: ${colors.white(lock.acquiredBy)}`, "lock");
        }
        if (lock.acquiredAt) {
          log.info(
            `  Since: ${
              colors.gray(new Date(lock.acquiredAt).toLocaleString())
            }`,
            "lock",
          );
        }
      }
    }

    if (unlockedHosts.length > 0) {
      log.success(
        `Unlocked hosts: ${
          colors.cyan(unlockedHosts.map((h) => h.host).join(", "))
        }`,
        "lock",
      );
    }
  } catch (error) {
    log.error(`Failed to check lock status:`, "lock");
    log.error(
      colors.red(error instanceof Error ? error.message : String(error)),
      "lock",
    );
    Deno.exit(1);
  } finally {
    cleanupSSHConnections(sshManagers);
  }
}

/**
 * Show detailed lock information
 */
async function showDetailedLockInfo(
  options: {
    config?: string;
    host?: string;
  } = {},
): Promise<void> {
  let sshManagers: SSHManager[] | undefined;

  try {
    log.info("Detailed Lock Information", "lock");

    // Cast options to GlobalOptions to access global options
    const globalOptions = options as unknown as GlobalOptions;
    const { config } = await loadConfig(globalOptions.configFile);
    const { targetHosts: _targetHosts, sshManagers: managers } =
      await setupLockSSHConnections(
        config,
        globalOptions,
      );
    sshManagers = managers;

    const lockStatuses = await checkLockStatus(sshManagers!);

    for (const status of lockStatuses) {
      log.info(`${colors.bold(colors.cyan(status.host || "unknown"))}`, "lock");
      log.info("─".repeat(40), "lock");

      if (status.locked) {
        log.error(`Status:      ${colors.red("LOCKED")}`, "lock");
        log.info(
          `Message:     ${colors.yellow(status.message || "No message")}`,
          "lock",
        );
        log.info(
          `Acquired by: ${colors.white(status.acquiredBy || "Unknown")}`,
          "lock",
        );
        log.info(
          `Acquired at: ${
            colors.gray(
              status.acquiredAt
                ? new Date(status.acquiredAt).toLocaleString()
                : "Unknown",
            )
          }`,
          "lock",
        );
        if (status.pid) {
          log.info(
            `Process ID:  ${colors.white(status.pid.toString())}`,
            "lock",
          );
        }
      } else {
        log.success(`Status:      ${colors.green("UNLOCKED")}`, "lock");
        log.info(`Available for deployment`, "lock");
      }

      log.info("", "lock");
    }
  } catch (error) {
    log.error(`Failed to show lock information:`, "lock");
    log.error(
      colors.red(error instanceof Error ? error.message : String(error)),
      "lock",
    );
    Deno.exit(1);
  } finally {
    cleanupSSHConnections(sshManagers);
  }
}

/**
 * Setup SSH connections to target hosts
 */
async function setupLockSSHConnections(
  config: Configuration,
  globalOptions: GlobalOptions,
): Promise<
  { targetHosts: string[]; sshManagers: SSHManager[] }
> {
  // Collect all unique hosts from services
  const allHosts = new Set<string>();
  for (const [, service] of config.services) {
    if (service.servers && service.servers.length > 0) {
      service.servers.forEach((server) => allHosts.add(server.host));
    }
  }

  let targetHosts = Array.from(allHosts);

  // Filter by specific hosts if requested
  if (globalOptions.hosts) {
    targetHosts = filterHostsByPatterns(targetHosts, globalOptions.hosts);

    if (targetHosts.length === 0) {
      throw new Error(
        `No hosts matched the pattern(s): ${globalOptions.hosts}`,
      );
    }
  }

  if (targetHosts.length === 0) {
    throw new Error("No remote hosts found in configuration");
  }

  const result = await setupSSHConnections(
    targetHosts,
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
    { allowPartialConnection: true },
  );

  log.success(
    `Connected: ${colors.green(result.connectedHosts.join(", "))}`,
    "lock",
  );

  return {
    targetHosts: result.connectedHosts,
    sshManagers: result.managers,
  };
}

/**
 * Check lock status on all hosts
 */
async function checkLockStatus(
  sshManagers: SSHManager[],
): Promise<LockInfo[]> {
  const results = await Promise.all(
    sshManagers.map(async (sshManager) => {
      const host = sshManager.getHost();
      try {
        const lockInfo = await getLockInfo(sshManager);
        return { ...lockInfo, host };
      } catch (error) {
        return {
          locked: false,
          host,
          error: error instanceof Error ? error.message : String(error),
        };
      }
    }),
  );

  return results;
}

/**
 * Get lock information from a single host
 */
async function getLockInfo(
  sshManager: SSHManager,
): Promise<LockInfo> {
  const lockFile = ".jiji/deploy.lock";

  // Check if lock file exists
  const checkResult = await sshManager.executeCommand(
    `test -f ${lockFile} && echo "exists" || echo "not_found"`,
  );

  if (!checkResult.success || checkResult.stdout.trim() === "not_found") {
    return { locked: false };
  }

  // Read lock file content
  const readResult = await sshManager.executeCommand(`cat ${lockFile}`);
  if (!readResult.success) {
    return { locked: false };
  }

  try {
    const lockData = JSON.parse(readResult.stdout.trim());
    return {
      locked: true,
      ...lockData,
    };
  } catch {
    // If we can't parse the lock file, treat it as unlocked
    return {
      locked: false,
    };
  }
}

/**
 * Create lock file on remote host
 */
async function createLockFile(
  sshManager: SSHManager,
  lockData: LockInfo,
): Promise<boolean> {
  const lockFile = ".jiji/deploy.lock";
  const lockContent = JSON.stringify(lockData, null, 2);

  // Ensure .jiji directory exists
  const mkdirResult = await sshManager.executeCommand("mkdir -p .jiji");
  if (!mkdirResult.success) {
    throw new Error(`Failed to create .jiji directory: ${mkdirResult.stderr}`);
  }

  // Create lock file
  const createResult = await sshManager.executeCommand(
    `cat > ${lockFile} << 'EOF'\n${lockContent}\nEOF`,
  );

  if (!createResult.success) {
    throw new Error(`Failed to create lock file: ${createResult.stderr}`);
  }

  return true;
}

/**
 * Remove lock file from remote host
 */
async function removeLockFile(
  sshManager: SSHManager,
): Promise<boolean> {
  const lockFile = ".jiji/deploy.lock";

  const result = await sshManager.executeCommand(`rm -f ${lockFile}`);
  if (!result.success) {
    throw new Error(`Failed to remove lock file: ${result.stderr}`);
  }

  return true;
}

/**
 * Clean up partial locks in case of failure
 */
async function cleanupPartialLocks(
  sshManagers: SSHManager[],
): Promise<void> {
  await Promise.all(
    sshManagers.map(async (sshManager) => {
      try {
        await removeLockFile(sshManager);
      } catch {
        // Ignore cleanup failures
      }
    }),
  );
}

/**
 * Get current user
 */
async function getCurrentUser(): Promise<string> {
  try {
    const result = await new Deno.Command("whoami", {
      stdout: "piped",
      stderr: "piped",
    }).output();

    if (result.success) {
      return new TextDecoder().decode(result.stdout).trim();
    }
  } catch {
    // Fallback methods
  }

  // Try environment variables
  return Deno.env.get("USER") || Deno.env.get("USERNAME") || "unknown";
}

/**
 * Clean up SSH connections
 */
function cleanupSSHConnections(
  sshManagers?: SSHManager[],
): void {
  if (sshManagers) {
    sshManagers.forEach((ssh) => {
      try {
        ssh.dispose();
      } catch {
        // Ignore cleanup errors
      }
    });
  }
}
