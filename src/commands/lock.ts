import { Command } from "@cliffy/command";
import { colors } from "@cliffy/ansi/colors";
import {
  cleanupSSHConnections,
  setupCommandContext,
} from "../utils/command_helpers.ts";
import { handleCommandError } from "../utils/error_handler.ts";
import type { GlobalOptions } from "../types.ts";
import type { SSHManager } from "../utils/ssh.ts";
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
  .action(async (options) => {
    await releaseLock(options as unknown as Record<string, unknown>);
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
  .action(async (options) => {
    await showDetailedLockInfo(options as unknown as Record<string, unknown>);
  });

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
  options: { force: boolean; timeout: number },
): Promise<void> {
  const globalOptions = options as unknown as GlobalOptions;
  let ctx: Awaited<ReturnType<typeof setupCommandContext>> | undefined;

  try {
    log.section("Acquiring Deployment Lock:");
    log.say(`- Message: ${message}`, 1);

    ctx = await setupCommandContext(globalOptions);
    const { config, sshManagers, targetHosts } = ctx;

    console.log("");
    for (const ssh of sshManagers) {
      log.remote(ssh.getHost(), ": Connected", { indent: 1 });
    }

    const auditLogger = createServerAuditLogger(sshManagers, config.project);

    log.section("Checking Existing Locks:");
    const lockStatuses = await checkLockStatus(sshManagers);
    const activeLocks = lockStatuses.filter((status) =>
      status.locked && !options.force
    );

    if (activeLocks.length > 0) {
      console.log();
      log.error("Deployment lock already exists:");

      for (const lock of activeLocks) {
        log.say(
          `${colors.cyan(lock.host || "unknown")}: ${
            colors.yellow(lock.message || "No message")
          }`,
          1,
        );
        if (lock.acquiredBy) {
          log.say(`Acquired by: ${colors.white(lock.acquiredBy)}`, 2);
        }
        if (lock.acquiredAt) {
          log.say(
            `Acquired at: ${
              colors.gray(new Date(lock.acquiredAt).toLocaleString())
            }`,
            2,
          );
        }
      }

      console.log();
      log.say(
        `Use ${colors.yellow("--force")} to override (use with caution)`,
        1,
      );
      Deno.exit(1);
    }

    // Acquire locks on all hosts
    log.section("Creating Lock Files:");

    const lockData: LockInfo = {
      locked: true,
      message,
      acquiredAt: new Date().toISOString(),
      acquiredBy: await getCurrentUser(),
      pid: Deno.pid,
    };

    const results = await Promise.all(
      sshManagers.map(async (sshManager) => {
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
      console.log();
      log.error("Failed to acquire locks on some hosts:");
      for (const failure of failures) {
        log.say(`${colors.cyan(failure.host)}: ${failure.error}`, 1);
      }

      log.section("Cleaning Up Partial Locks:");
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

    log.success("\nDeployment lock acquired successfully!", 0);
    log.say(`Message: ${colors.white(message)}`, 1);
    log.say(`Hosts: ${colors.cyan(targetHosts.join(", "))}`, 1);
    log.say(`To release: ${colors.yellow("jiji lock release")}`, 1);
  } catch (error) {
    await handleCommandError(error, {
      operation: "Lock acquisition",
      component: "lock",
      sshManagers: ctx?.sshManagers,
      projectName: ctx?.config?.project,
      targetHosts: ctx?.targetHosts,
    });
  } finally {
    if (ctx?.sshManagers) {
      cleanupSSHConnections(ctx.sshManagers);
    }
  }
}

/**
 * Release a deployment lock
 */
async function releaseLock(options: Record<string, unknown>): Promise<void> {
  const globalOptions = options as unknown as GlobalOptions;
  let ctx: Awaited<ReturnType<typeof setupCommandContext>> | undefined;

  try {
    log.section("Releasing Deployment Lock:");

    ctx = await setupCommandContext(globalOptions);
    const { config, sshManagers } = ctx;

    console.log("");
    for (const ssh of sshManagers) {
      log.remote(ssh.getHost(), ": Connected", { indent: 1 });
    }

    const auditLogger = createServerAuditLogger(sshManagers, config.project);

    log.section("Checking Existing Locks:");
    const lockStatuses = await checkLockStatus(sshManagers);
    const activeLocks = lockStatuses.filter((status) => status.locked);

    if (activeLocks.length === 0) {
      log.warn("\nNo deployment locks found", 0);
      return;
    }

    log.section("Removing Lock Files:");

    const results = await Promise.all(
      sshManagers.map(async (sshManager) => {
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
      console.log();
      log.warn("Failed to release locks on some hosts:");
      for (const failure of failures) {
        log.say(`${colors.cyan(failure.host)}: ${failure.error}`, 1);
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

      log.success("\nDeployment lock released successfully!", 0);
      log.say(
        `Hosts: ${colors.cyan(successes.map((s) => s.host).join(", "))}`,
        1,
      );
    }
  } catch (error) {
    await handleCommandError(error, {
      operation: "Lock release",
      component: "lock",
      sshManagers: ctx?.sshManagers,
      projectName: ctx?.config?.project,
      targetHosts: ctx?.targetHosts,
    });
  } finally {
    if (ctx?.sshManagers) {
      cleanupSSHConnections(ctx.sshManagers);
    }
  }
}

/**
 * Show lock status for all hosts
 */
async function showLockStatus(options: { json: boolean }): Promise<void> {
  const globalOptions = options as unknown as GlobalOptions;
  let ctx: Awaited<ReturnType<typeof setupCommandContext>> | undefined;

  try {
    ctx = await setupCommandContext(globalOptions);
    const { sshManagers } = ctx;

    const lockStatuses = await checkLockStatus(sshManagers);

    if (options.json) {
      log.say(
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
      );
      return;
    }

    log.section("Deployment Lock Status:");

    const activeLocks = lockStatuses.filter((s) => s.locked);
    const unlockedHosts = lockStatuses.filter((s) => !s.locked);

    if (activeLocks.length === 0) {
      log.say("- No active deployment locks", 1);
    } else {
      log.say(`- ${activeLocks.length} active lock(s)`, 1);
      console.log();
      for (const lock of activeLocks) {
        log.say(`${colors.cyan(lock.host || "unknown")}:`, 1);
        log.say(`├── Status: ${colors.red("LOCKED")}`, 2);
        if (lock.message) {
          log.say(`├── Message: ${colors.yellow(lock.message)}`, 2);
        }
        if (lock.acquiredBy) {
          log.say(`├── Owner: ${colors.white(lock.acquiredBy)}`, 2);
        }
        if (lock.acquiredAt) {
          log.say(
            `└── Since: ${
              colors.gray(new Date(lock.acquiredAt).toLocaleString())
            }`,
            2,
          );
        }
        console.log();
      }
    }

    if (unlockedHosts.length > 0) {
      log.say(
        `- Unlocked hosts: ${
          colors.cyan(unlockedHosts.map((h) => h.host).join(", "))
        }`,
        1,
      );
    }
  } catch (error) {
    await handleCommandError(error, {
      operation: "Lock status",
      component: "lock",
      sshManagers: ctx?.sshManagers,
      projectName: ctx?.config?.project,
      targetHosts: ctx?.targetHosts,
    });
  } finally {
    if (ctx?.sshManagers) {
      cleanupSSHConnections(ctx.sshManagers);
    }
  }
}

/**
 * Show detailed lock information
 */
async function showDetailedLockInfo(
  options: Record<string, unknown>,
): Promise<void> {
  const globalOptions = options as unknown as GlobalOptions;
  let ctx: Awaited<ReturnType<typeof setupCommandContext>> | undefined;

  try {
    log.section("Detailed Lock Information:");

    ctx = await setupCommandContext(globalOptions);
    const { sshManagers } = ctx;

    console.log("");
    for (const ssh of sshManagers) {
      log.remote(ssh.getHost(), ": Connected", { indent: 1 });
    }

    log.section("Lock Details:");

    const lockStatuses = await checkLockStatus(sshManagers);

    for (const status of lockStatuses) {
      log.say(`${colors.bold(colors.cyan(status.host || "unknown"))}`, 1);

      if (status.locked) {
        log.say(`├── Status:      ${colors.red("LOCKED")}`, 2);
        log.say(
          `├── Message:     ${colors.yellow(status.message || "No message")}`,
          2,
        );
        log.say(
          `├── Acquired by: ${colors.white(status.acquiredBy || "Unknown")}`,
          2,
        );
        const acquiredTime = status.acquiredAt
          ? new Date(status.acquiredAt).toLocaleString()
          : "Unknown";

        if (status.pid) {
          log.say(
            `├── Acquired at: ${colors.gray(acquiredTime)}`,
            2,
          );
          log.say(
            `└── Process ID:  ${colors.white(status.pid.toString())}`,
            2,
          );
        } else {
          log.say(
            `└── Acquired at: ${colors.gray(acquiredTime)}`,
            2,
          );
        }
      } else {
        log.say(`├── Status:      ${colors.green("UNLOCKED")}`, 2);
        log.say(`└── Available for deployment`, 2);
      }

      console.log();
    }
  } catch (error) {
    await handleCommandError(error, {
      operation: "Lock info",
      component: "lock",
      sshManagers: ctx?.sshManagers,
      projectName: ctx?.config?.project,
      targetHosts: ctx?.targetHosts,
    });
  } finally {
    if (ctx?.sshManagers) {
      cleanupSSHConnections(ctx.sshManagers);
    }
  }
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
async function getLockInfo(sshManager: SSHManager): Promise<LockInfo> {
  const lockFile = ".jiji/deploy.lock";

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
    return { locked: false };
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

  const mkdirResult = await sshManager.executeCommand("mkdir -p .jiji");
  if (!mkdirResult.success) {
    throw new Error(`Failed to create .jiji directory: ${mkdirResult.stderr}`);
  }

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
async function removeLockFile(sshManager: SSHManager): Promise<boolean> {
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
async function cleanupPartialLocks(sshManagers: SSHManager[]): Promise<void> {
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
