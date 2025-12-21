import { join } from "@std/path";
import type { SSHManager } from "./ssh.ts";
import { executeHostOperations } from "./promise_helpers.ts";

export interface LockInfo {
  locked: boolean;
  message?: string;
  acquiredAt?: string;
  acquiredBy?: string;
  host?: string;
  pid?: number;
  version?: string;
}

export interface LockManager {
  acquire(message: string): Promise<boolean>;
  release(): Promise<boolean>;
  status(): Promise<LockInfo>;
  isLocked(): Promise<boolean>;
}

/**
 * Remote lock manager for SSH-based deployments
 */
export class RemoteLockManager implements LockManager {
  private sshManager: SSHManager;
  private projectName: string;
  private lockDir: string;
  private lockFile: string;

  constructor(sshManager: SSHManager, projectName: string) {
    this.sshManager = sshManager;
    this.projectName = projectName;
    this.lockDir = `.jiji/${projectName}`;
    this.lockFile = `.jiji/${projectName}/deploy.lock`;
  }

  async acquire(message: string): Promise<boolean> {
    try {
      // Check if already locked
      const currentStatus = await this.status();
      if (currentStatus.locked) {
        return false;
      }

      // Create lock data
      const lockData: LockInfo = {
        locked: true,
        message,
        acquiredAt: new Date().toISOString(),
        acquiredBy: await this.getCurrentUser(),
        host: this.sshManager.getHost(),
        pid: Deno.pid,
        version: "1.0",
      };

      // Ensure .jiji directory exists
      await this.ensureDirectory();

      // Create lock file atomically
      const lockContent = JSON.stringify(lockData, null, 2);
      const tempFile = `${this.lockFile}.tmp`;

      // Write to temp file first
      const writeResult = await this.sshManager.executeCommand(
        `cat > ${tempFile} << 'EOF'\n${lockContent}\nEOF`,
      );

      if (!writeResult.success) {
        throw new Error(`Failed to write lock file: ${writeResult.stderr}`);
      }

      // Atomic move to actual lock file
      const moveResult = await this.sshManager.executeCommand(
        `mv ${tempFile} ${this.lockFile}`,
      );

      if (!moveResult.success) {
        // Clean up temp file
        await this.sshManager.executeCommand(`rm -f ${tempFile}`);
        throw new Error(`Failed to create lock file: ${moveResult.stderr}`);
      }

      return true;
    } catch (error) {
      throw new Error(
        `Failed to acquire lock: ${
          error instanceof Error ? error.message : String(error)
        }`,
      );
    }
  }

  async release(): Promise<boolean> {
    try {
      const result = await this.sshManager.executeCommand(
        `rm -f ${this.lockFile}`,
      );
      return result.success;
    } catch {
      return false;
    }
  }

  async status(): Promise<LockInfo> {
    try {
      // Check if lock file exists
      const checkResult = await this.sshManager.executeCommand(
        `test -f ${this.lockFile} && echo "exists" || echo "not_found"`,
      );

      if (!checkResult.success || checkResult.stdout.trim() === "not_found") {
        return { locked: false, host: this.sshManager.getHost() };
      }

      // Read lock file content
      const readResult = await this.sshManager.executeCommand(
        `cat ${this.lockFile}`,
      );
      if (!readResult.success) {
        return { locked: false, host: this.sshManager.getHost() };
      }

      try {
        const lockData = JSON.parse(readResult.stdout.trim());
        return {
          ...lockData,
          host: this.sshManager.getHost(),
        };
      } catch {
        // If we can't parse the lock file, treat it as unlocked
        return { locked: false, host: this.sshManager.getHost() };
      }
    } catch {
      return { locked: false, host: this.sshManager.getHost() };
    }
  }

  async isLocked(): Promise<boolean> {
    const status = await this.status();
    return status.locked;
  }

  private async ensureDirectory(): Promise<void> {
    const result = await this.sshManager.executeCommand(
      `mkdir -p ${this.lockDir}`,
    );
    if (!result.success) {
      throw new Error(
        `Failed to create ${this.lockDir} directory: ${result.stderr}`,
      );
    }
  }

  private async getCurrentUser(): Promise<string> {
    try {
      const result = await this.sshManager.executeCommand("whoami");
      if (result.success) {
        return result.stdout.trim();
      }
    } catch {
      // Fallback
    }
    return "unknown";
  }

  getHost(): string {
    return this.sshManager.getHost();
  }
}

/**
 * Local lock manager for local operations
 */
export class LocalLockManager implements LockManager {
  private lockFile: string;
  private lockDir: string;
  private projectName: string;

  constructor(projectName: string, projectRoot: string = Deno.cwd()) {
    this.projectName = projectName;
    this.lockDir = join(projectRoot, ".jiji", projectName);
    this.lockFile = join(this.lockDir, "deploy.lock");
  }

  async acquire(message: string): Promise<boolean> {
    try {
      // Check if already locked
      const currentStatus = await this.status();
      if (currentStatus.locked) {
        return false;
      }

      // Create lock data
      const lockData: LockInfo = {
        locked: true,
        message,
        acquiredAt: new Date().toISOString(),
        acquiredBy: await this.getCurrentUser(),
        host: "localhost",
        pid: Deno.pid,
        version: "1.0",
      };

      // Ensure directory exists
      await Deno.mkdir(this.lockDir, { recursive: true });

      // Create lock file atomically
      const tempFile = `${this.lockFile}.tmp`;
      await Deno.writeTextFile(tempFile, JSON.stringify(lockData, null, 2));

      // Atomic move
      await Deno.rename(tempFile, this.lockFile);

      return true;
    } catch (error) {
      throw new Error(
        `Failed to acquire local lock: ${
          error instanceof Error ? error.message : String(error)
        }`,
      );
    }
  }

  async release(): Promise<boolean> {
    try {
      await Deno.remove(this.lockFile);
      return true;
    } catch {
      return false;
    }
  }

  async status(): Promise<LockInfo> {
    try {
      const content = await Deno.readTextFile(this.lockFile);
      const lockData = JSON.parse(content);
      return {
        ...lockData,
        host: "localhost",
      };
    } catch {
      return { locked: false, host: "localhost" };
    }
  }

  async isLocked(): Promise<boolean> {
    const status = await this.status();
    return status.locked;
  }

  private async getCurrentUser(): Promise<string> {
    try {
      const result = await new Deno.Command("whoami", {
        stdout: "piped",
        stderr: "piped",
      }).output();

      if (result.success) {
        return new TextDecoder().decode(result.stdout).trim();
      }
    } catch {
      // Fallback
    }

    return Deno.env.get("USER") || Deno.env.get("USERNAME") || "unknown";
  }

  getLockFilePath(): string {
    return this.lockFile;
  }
}

/**
 * Multi-host lock manager
 */
export class MultiHostLockManager {
  private lockManagers: RemoteLockManager[];
  private projectName: string;

  constructor(sshManagers: SSHManager[], projectName: string) {
    this.projectName = projectName;
    this.lockManagers = sshManagers.map((ssh) =>
      new RemoteLockManager(ssh, projectName)
    );
  }

  async acquireAll(message: string): Promise<{
    success: boolean;
    results: { host: string; success: boolean; error?: string }[];
  }> {
    // Create host operations for error collection
    const hostOperations = this.lockManagers.map((manager) => ({
      host: manager.getHost(),
      operation: async () => {
        const success = await manager.acquire(message);
        return { host: manager.getHost(), success };
      },
    }));

    // Execute with error collection
    const aggregatedResults = await executeHostOperations(hostOperations);

    const results: { host: string; success: boolean; error?: string }[] = [
      ...aggregatedResults.results,
    ];

    for (const { host, error } of aggregatedResults.hostErrors) {
      results.push({
        host,
        success: false,
        error: error.message,
      });
    }

    const failures = results.filter((r) => !r.success);

    if (failures.length > 0) {
      await this.cleanupPartialLocks(results.filter((r) => r.success));
      return { success: false, results };
    }

    return { success: true, results };
  }

  async releaseAll(): Promise<{
    success: boolean;
    results: { host: string; success: boolean; error?: string }[];
  }> {
    const hostOperations = this.lockManagers.map((manager) => ({
      host: manager.getHost(),
      operation: async () => {
        const success = await manager.release();
        return { host: manager.getHost(), success };
      },
    }));
    const aggregatedResults = await executeHostOperations(hostOperations);

    const results: { host: string; success: boolean; error?: string }[] = [
      ...aggregatedResults.results,
    ];

    for (const { host, error } of aggregatedResults.hostErrors) {
      results.push({
        host,
        success: false,
        error: error.message,
      });
    }

    return {
      success: results.every((r) => r.success),
      results,
    };
  }

  async statusAll(): Promise<LockInfo[]> {
    const hostOperations = this.lockManagers.map((manager) => ({
      host: manager.getHost(),
      operation: async () => await manager.status(),
    }));

    const aggregatedResults = await executeHostOperations(hostOperations);
    const results: LockInfo[] = [...aggregatedResults.results];

    for (const { host, error } of aggregatedResults.hostErrors) {
      results.push({
        locked: false,
        host,
        error: error.message,
      } as LockInfo);
    }

    return results;
  }

  async isAnyLocked(): Promise<boolean> {
    const statuses = await this.statusAll();
    return statuses.some((status) => status.locked);
  }

  private async cleanupPartialLocks(
    successfulResults: { host: string; success: boolean }[],
  ): Promise<void> {
    const hostsToCleanup = successfulResults.map((r) => r.host);

    for (const manager of this.lockManagers) {
      if (hostsToCleanup.includes(manager.getHost())) {
        try {
          await manager.release();
        } catch {
          // Ignore cleanup failures
        }
      }
    }
  }

  getHosts(): string[] {
    return this.lockManagers.map((manager) => manager.getHost());
  }
}

/**
 * Create appropriate lock manager based on context
 */
export function createLockManager(
  projectName: string,
  sshManagers?: SSHManager | SSHManager[],
  projectRoot?: string,
): LockManager | MultiHostLockManager {
  if (!sshManagers) {
    return new LocalLockManager(projectName, projectRoot);
  }

  if (Array.isArray(sshManagers)) {
    if (sshManagers.length === 1) {
      return new RemoteLockManager(sshManagers[0], projectName);
    }
    return new MultiHostLockManager(sshManagers, projectName);
  }

  return new RemoteLockManager(sshManagers, projectName);
}

/**
 * Lock decorator for functions that need exclusive access
 */
export function withLock<T extends unknown[], R>(
  lockManager: LockManager,
  message: string,
) {
  return function (
    _target: unknown,
    _propertyKey: string,
    descriptor: PropertyDescriptor,
  ) {
    const originalMethod = descriptor.value!;

    descriptor.value = async function (...args: T): Promise<R> {
      const acquired = await lockManager.acquire(message);
      if (!acquired) {
        const status = await lockManager.status();
        throw new Error(
          `Cannot acquire lock: ${status.message || "Already locked"}`,
        );
      }

      try {
        return await originalMethod.apply(this, args);
      } finally {
        await lockManager.release();
      }
    };

    return descriptor;
  };
}

/**
 * Check if a process is still running (for stale lock detection)
 */
export async function isProcessRunning(pid: number): Promise<boolean> {
  try {
    const result = await new Deno.Command("kill", {
      args: ["-0", pid.toString()],
      stdout: "null",
      stderr: "null",
    }).output();

    return result.success;
  } catch {
    return false;
  }
}

/**
 * Detect and clean up stale locks
 */
export async function cleanupStaleLocks(
  lockManager: LockManager,
): Promise<boolean> {
  try {
    const status = await lockManager.status();

    if (!status.locked || !status.pid) {
      return false;
    }

    const processRunning = await isProcessRunning(status.pid);

    if (!processRunning) {
      await lockManager.release();
      return true;
    }

    return false;
  } catch {
    return false;
  }
}
