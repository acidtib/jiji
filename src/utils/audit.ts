import { join } from "@std/path";
import type { SSHManager } from "./ssh.ts";

export interface AuditEntry {
  timestamp: string;
  action: string;
  details?: Record<string, unknown>;
  user?: string;
  host?: string;
  status: "started" | "success" | "failed" | "warning";
  message?: string;
}

export interface RemoteAuditResult {
  host: string;
  success: boolean;
  error?: string;
}

export class RemoteAuditLogger {
  private sshManager: SSHManager;
  private auditDir: string = ".jiji";
  private auditFile: string = ".jiji/audit.txt";

  constructor(sshManager: SSHManager) {
    this.sshManager = sshManager;
  }

  /**
   * Initialize the audit directory and file on the remote server
   */
  async initRemoteAudit(): Promise<boolean> {
    try {
      const host = this.sshManager.getHost();

      // Check if audit directory exists, create if not
      const checkDirResult = await this.sshManager.executeCommand(
        `test -d ${this.auditDir} || mkdir -p ${this.auditDir}`,
      );

      if (!checkDirResult.success) {
        // console.warn(
        //   `⚠️  Failed to create audit directory on ${host}: ${checkDirResult.stderr}`,
        // );
        return false;
      }

      // Check if audit file exists, if not create it with header
      const checkFileResult = await this.sshManager.executeCommand(
        `test -f ${this.auditFile}`,
      );

      if (!checkFileResult.success) {
        const header = [
          "# Jiji Audit Trail",
          `# Generated on ${new Date().toISOString()}`,
          `# Server: ${host}`,
          "# Format: [TIMESTAMP] [STATUS] ACTION - MESSAGE",
          "",
        ].join("\\n");

        const createFileResult = await this.sshManager.executeCommand(
          `echo -e "${header}" > ${this.auditFile}`,
        );

        if (!createFileResult.success) {
          // console.warn(
          //   `⚠️  Failed to create audit file on ${host}: ${createFileResult.stderr}`,
          // );
          return false;
        }
      }

      return true;
    } catch (_error) {
      // console.warn(
      //   `⚠️  Failed to initialize remote audit: ${
      //     error instanceof Error ? error.message : String(error)
      //   }`,
      // );
      return false;
    }
  }

  /**
   * Log an audit entry to the remote server
   */
  async logRemote(
    entry: Omit<AuditEntry, "timestamp" | "host">,
  ): Promise<boolean> {
    try {
      const host = this.sshManager.getHost();

      // Initialize audit system if needed
      const initialized = await this.initRemoteAudit();
      if (!initialized) {
        return false;
      }

      const fullEntry: AuditEntry = {
        timestamp: new Date().toISOString(),
        host,
        ...entry,
      };

      const formattedEntry = this.formatEntry(fullEntry);

      // Escape the entry for shell command
      const escapedEntry = formattedEntry.replace(/"/g, '\\"').replace(
        /'/g,
        "\\'",
      );

      const result = await this.sshManager.executeCommand(
        `echo "${escapedEntry}" >> ${this.auditFile}`,
      );

      if (!result.success) {
        // console.warn(
        //   `⚠️  Failed to write audit entry to ${host}: ${result.stderr}`,
        // );
        return false;
      }

      return true;
    } catch (_error) {
      // console.warn(
      //   `Failed to log audit entry: ${
      //     error instanceof Error ? error.message : String(error)
      //   }`,
      // );
      return false;
    }
  }

  /**
   * Read recent audit entries from the remote server
   */
  async getRemoteEntries(limit: number = 50): Promise<string[]> {
    try {
      const result = await this.sshManager.executeCommand(
        `tail -n ${
          limit * 2
        } ${this.auditFile} 2>/dev/null | grep -v '^#' | grep -v '^$' | tail -n ${limit}`,
      );

      if (!result.success) {
        return [];
      }

      return result.stdout.split("\n").filter((line) => line.trim());
    } catch {
      return [];
    }
  }

  /**
   * Format an audit entry for writing to file
   */
  private formatEntry(entry: AuditEntry): string {
    const timestamp = entry.timestamp;
    const status = entry.status.toUpperCase().padEnd(8);
    const action = entry.action.toUpperCase();
    const message = entry.message || "";

    let line = `[${timestamp}] [${status}] ${action}`;
    if (message) {
      line += ` - ${message}`;
    }

    // Add details as JSON on next line if present
    if (entry.details && Object.keys(entry.details).length > 0) {
      const details = JSON.stringify(entry.details, null, 0);
      line += `\\n    Details: ${details}`;
    }

    return line;
  }

  /**
   * Get the remote host
   */
  getHost(): string {
    return this.sshManager.getHost();
  }
}

export class AuditAggregator {
  private sshManagers: SSHManager[];

  constructor(sshManagers: SSHManager[]) {
    this.sshManagers = sshManagers;
  }

  /**
   * Log an entry to all connected servers
   */
  async logToAllServers(
    entry: Omit<AuditEntry, "timestamp" | "host">,
  ): Promise<RemoteAuditResult[]> {
    const results: RemoteAuditResult[] = [];

    for (const sshManager of this.sshManagers) {
      const logger = new RemoteAuditLogger(sshManager);
      const host = sshManager.getHost();

      try {
        if (!sshManager.isConnected()) {
          await sshManager.connect();
        }

        const success = await logger.logRemote(entry);
        results.push({ host, success });
      } catch (error) {
        results.push({
          host,
          success: false,
          error: error instanceof Error ? error.message : String(error),
        });
      }
    }

    return results;
  }

  /**
   * Get aggregated audit entries from all servers
   */
  async getAggregatedEntries(
    limit: number = 50,
  ): Promise<{ host: string; entries: string[] }[]> {
    const results: { host: string; entries: string[] }[] = [];

    for (const sshManager of this.sshManagers) {
      const logger = new RemoteAuditLogger(sshManager);
      const host = sshManager.getHost();

      try {
        if (!sshManager.isConnected()) {
          await sshManager.connect();
        }

        const entries = await logger.getRemoteEntries(limit);
        results.push({ host, entries });
      } catch (_error) {
        // console.warn(
        //   `⚠️  Failed to get audit entries from ${host}: ${
        //     error instanceof Error ? error.message : String(error)
        //   }`,
        // );
        results.push({ host, entries: [] });
      }
    }

    return results;
  }
}

/**
 * Enhanced audit logger for server operations
 */
export class ServerAuditLogger {
  private auditAggregator?: AuditAggregator;
  private singleLogger?: RemoteAuditLogger;

  constructor(sshManagers: SSHManager | SSHManager[]) {
    if (Array.isArray(sshManagers)) {
      this.auditAggregator = new AuditAggregator(sshManagers);
    } else {
      this.singleLogger = new RemoteAuditLogger(sshManagers);
    }
  }

  /**
   * Log a server bootstrap start event
   */
  async logBootstrapStart(
    hosts: string[],
    engine: string,
  ): Promise<{ host: string; success: boolean }[]> {
    const entry = {
      action: "server_bootstrap",
      status: "started" as const,
      message: `Bootstrap started for ${hosts.length} host(s) with ${engine}`,
      details: {
        hosts,
        engine,
      },
    };

    return await this.logEntryWithResults(entry);
  }

  /**
   * Log a server bootstrap success event
   */
  async logBootstrapSuccess(
    hosts: string[],
    engine: string,
  ): Promise<{ host: string; success: boolean }[]> {
    const entry = {
      action: "server_bootstrap",
      status: "success" as const,
      message: `Bootstrap completed successfully for ${hosts.length} host(s)`,
      details: {
        hosts,
        engine,
      },
    };

    return await this.logEntryWithResults(entry);
  }

  /**
   * Log a server bootstrap failure event
   */
  async logBootstrapFailure(
    error: string,
    hosts?: string[],
    engine?: string,
  ): Promise<{ host: string; success: boolean }[]> {
    const entry = {
      action: "server_bootstrap",
      status: "failed" as const,
      message: `Bootstrap failed: ${error}`,
      details: {
        hosts,
        engine,
        error,
      },
    };

    return await this.logEntryWithResults(entry);
  }

  /**
   * Log engine installation events
   */
  async logEngineInstall(
    engine: string,
    status: "started" | "success" | "failed",
    message?: string,
  ): Promise<void> {
    const entry = {
      action: "engine_install",
      status,
      message: message || `Engine ${engine} installation ${status}`,
      details: {
        engine,
      },
    };

    await this.logEntry(entry);
  }

  /**
   * Log service deployment events
   */
  async logServiceDeploy(
    serviceName: string,
    status: "started" | "success" | "failed",
    message?: string,
  ): Promise<void> {
    const entry = {
      action: "service_deploy",
      status,
      message: message || `Service ${serviceName} deployment ${status}`,
      details: {
        serviceName,
      },
    };

    await this.logEntry(entry);
  }

  /**
   * Log configuration changes
   */
  async logConfigChange(
    configPath: string,
    action: "loaded" | "updated" | "created",
  ): Promise<void> {
    const entry = {
      action: "config_change",
      status: "success" as const,
      message: `Configuration ${action}: ${configPath}`,
      details: {
        configPath,
        changeType: action,
      },
    };

    await this.logEntry(entry);
  }

  /**
   * Log custom command execution events
   */
  async logCustomCommand(
    command: string,
    status: "started" | "success" | "failed",
    message?: string,
  ): Promise<void> {
    const entry = {
      action: "custom_command",
      status,
      message: message || `Custom command ${status}: ${command}`,
      details: {
        command,
      },
    };

    await this.logEntry(entry);
  }

  /**
   * Log a custom audit entry
   */
  async logEntry(entry: Omit<AuditEntry, "timestamp" | "host">): Promise<void> {
    await this.logEntryWithResults(entry);
  }

  /**
   * Log a custom audit entry and return results
   */
  async logEntryWithResults(
    entry: Omit<AuditEntry, "timestamp" | "host">,
  ): Promise<{ host: string; success: boolean }[]> {
    if (this.auditAggregator) {
      const results = await this.auditAggregator.logToAllServers(entry);
      const failures = results.filter((r) => !r.success);

      if (failures.length > 0) {
        // console.warn(
        //   `Failed to log audit entry to ${failures.length} server(s):`,
        // );
        // failures.forEach((f) =>
        //   console.warn(`   • ${f.host}: ${f.error || "Unknown error"}`)
        // );
      }
      return results.map((r) => ({ host: r.host, success: r.success }));
    } else if (this.singleLogger) {
      try {
        await this.singleLogger.logRemote(entry);
        return [{ host: this.singleLogger.getHost(), success: true }];
      } catch {
        return [{ host: this.singleLogger.getHost(), success: false }];
      }
    }
    return [];
  }

  /**
   * Get recent audit entries
   */
  async getRecentEntries(
    limit: number = 50,
  ): Promise<{ host: string; entries: string[] }[]> {
    if (this.auditAggregator) {
      return await this.auditAggregator.getAggregatedEntries(limit);
    } else if (this.singleLogger) {
      const entries = await this.singleLogger.getRemoteEntries(limit);
      return [{ host: this.singleLogger.getHost(), entries }];
    }
    return [];
  }

  /**
   * Get hosts being audited
   */
  getHosts(): string[] {
    if (this.auditAggregator) {
      return this.auditAggregator["sshManagers"].map((ssh) => ssh.getHost());
    } else if (this.singleLogger) {
      return [this.singleLogger.getHost()];
    }
    return [];
  }
}

/**
 * Create a server audit logger
 */
export const createServerAuditLogger = (
  sshManagers: SSHManager | SSHManager[],
): ServerAuditLogger => {
  return new ServerAuditLogger(sshManagers);
};

/**
 * Local audit logger for operations that don't involve remote servers
 */
export class LocalAuditLogger {
  private auditDir: string;
  private auditFile: string;

  constructor(projectRoot: string = Deno.cwd()) {
    this.auditDir = join(projectRoot, ".jiji");
    this.auditFile = join(this.auditDir, "audit.txt");
  }

  /**
   * Initialize the local audit directory and file
   */
  async init(): Promise<void> {
    try {
      await Deno.mkdir(this.auditDir, { recursive: true });

      try {
        await Deno.stat(this.auditFile);
      } catch {
        const header = `# Jiji Local Audit Trail\n# Generated on ${
          new Date().toISOString()
        }\n# Format: [TIMESTAMP] [STATUS] ACTION - MESSAGE\n\n`;
        await Deno.writeTextFile(this.auditFile, header);
      }
    } catch (error) {
      throw new Error(
        `Failed to initialize local audit trail: ${
          error instanceof Error ? error.message : String(error)
        }`,
      );
    }
  }

  /**
   * Log a local audit entry (for operations that don't involve remote servers)
   */
  async log(entry: Omit<AuditEntry, "timestamp">): Promise<void> {
    try {
      await this.init();

      const fullEntry: AuditEntry = {
        timestamp: new Date().toISOString(),
        ...entry,
      };

      const formattedEntry = this.formatEntry(fullEntry);
      await Deno.writeTextFile(this.auditFile, formattedEntry + "\n", {
        append: true,
      });
    } catch (_error) {
      // console.warn(
      //   `⚠️  Failed to write local audit entry: ${
      //     error instanceof Error ? error.message : String(error)
      //   }`,
      // );
    }
  }

  /**
   * Format an audit entry for writing to file
   */
  private formatEntry(entry: AuditEntry): string {
    const timestamp = entry.timestamp;
    const status = entry.status.toUpperCase().padEnd(8);
    const action = entry.action.toUpperCase();
    const host = entry.host ? ` [${entry.host}]` : "";
    const message = entry.message || "";

    let line = `[${timestamp}] [${status}] ${action}${host}`;
    if (message) {
      line += ` - ${message}`;
    }

    if (entry.details && Object.keys(entry.details).length > 0) {
      const details = JSON.stringify(entry.details, null, 0);
      line += `\n    Details: ${details}`;
    }

    return line;
  }

  /**
   * Get the audit file path
   */
  getAuditFilePath(): string {
    return this.auditFile;
  }
}

/**
 * Create a local audit logger
 */
export const createLocalAuditLogger = (
  projectRoot?: string,
): LocalAuditLogger => {
  return new LocalAuditLogger(projectRoot);
};
