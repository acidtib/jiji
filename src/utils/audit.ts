import { join } from "@std/path";
import type { SSHManager } from "./ssh.ts";
import { executeHostOperations } from "./promise_helpers.ts";
import { log, Logger } from "./logger.ts";
import type { AuditEntry, RemoteAuditResult } from "../types.ts";

export class RemoteAuditLogger {
  private sshManager: SSHManager;
  private projectName: string;
  private auditDir: string;
  private auditFile: string;
  private logger: Logger;

  constructor(sshManager: SSHManager, projectName: string) {
    this.sshManager = sshManager;
    this.projectName = projectName;
    this.auditDir = `.jiji/${projectName}`;
    this.auditFile = `.jiji/${projectName}/audit.txt`;
    this.logger = new Logger({ prefix: "audit" });
  }

  /**
   * Initialize the audit directory and file on the remote server
   */
  async initRemoteAudit(): Promise<boolean> {
    try {
      const host = this.sshManager.getHost();

      const checkDirResult = await this.sshManager.executeCommand(
        `test -d ${this.auditDir} || mkdir -p ${this.auditDir}`,
      );

      if (!checkDirResult.success) {
        this.logger.warn(
          `Failed to create audit directory on ${host}: ${checkDirResult.stderr}`,
        );
        return false;
      }

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
          this.logger.warn(
            `Failed to create audit file on ${host}: ${createFileResult.stderr}`,
          );
          return false;
        }
      }

      return true;
    } catch (error) {
      this.logger.warn(
        `Failed to initialize remote audit: ${
          error instanceof Error ? error.message : String(error)
        }`,
      );
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

      const escapedEntry = formattedEntry.replace(/"/g, '\\"').replace(
        /'/g,
        "\\'",
      );

      const result = await this.sshManager.executeCommand(
        `echo "${escapedEntry}" >> ${this.auditFile}`,
      );

      if (!result.success) {
        this.logger.warn(
          `Failed to write audit entry to ${host}: ${result.stderr}`,
        );
        return false;
      }

      return true;
    } catch (error) {
      this.logger.warn(
        `Failed to log audit entry: ${
          error instanceof Error ? error.message : String(error)
        }`,
      );
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
  private projectName: string;

  constructor(sshManagers: SSHManager[], projectName: string) {
    this.sshManagers = sshManagers;
    this.projectName = projectName;
  }

  /**
   * Log an entry to all connected servers with enhanced error collection
   */
  async logToAllServers(
    entry: Omit<AuditEntry, "timestamp" | "host">,
  ): Promise<RemoteAuditResult[]> {
    const hostOperations = this.sshManagers.map((sshManager) => ({
      host: sshManager.getHost(),
      operation: async (): Promise<RemoteAuditResult> => {
        const logger = new RemoteAuditLogger(sshManager, this.projectName);
        const host = sshManager.getHost();

        if (!sshManager.isConnected()) {
          await sshManager.connect();
        }

        const success = await logger.logRemote(entry);
        return { host, success };
      },
    }));

    const aggregatedResults = await executeHostOperations(hostOperations);
    const results = [...aggregatedResults.results];

    for (const { host, error } of aggregatedResults.hostErrors) {
      results.push({
        host,
        success: false,
        error: error.message,
      });
    }

    if (aggregatedResults.errorCount > 0) {
      log.warn(
        `Audit logging completed with ${aggregatedResults.errorCount} failures out of ${results.length} servers`,
        "audit",
      );
    }

    return results;
  }

  /**
   * Get aggregated audit entries from all servers with enhanced error collection
   */
  async getAggregatedEntries(
    limit: number = 50,
  ): Promise<{ host: string; entries: string[] }[]> {
    const hostOperations = this.sshManagers.map((sshManager) => ({
      host: sshManager.getHost(),
      operation: async (): Promise<{ host: string; entries: string[] }> => {
        const logger = new RemoteAuditLogger(sshManager, this.projectName);
        const host = sshManager.getHost();

        if (!sshManager.isConnected()) {
          await sshManager.connect();
        }

        const entries = await logger.getRemoteEntries(limit);
        return { host, entries };
      },
    }));

    const aggregatedResults = await executeHostOperations(hostOperations);
    const results = [...aggregatedResults.results];

    for (const { host, error } of aggregatedResults.hostErrors) {
      log.warn(
        `Failed to get audit entries from ${host}: ${error.message}`,
        "audit",
      );
      results.push({ host, entries: [] });
    }

    if (aggregatedResults.errorCount > 0) {
      log.warn(
        `Audit retrieval completed with ${aggregatedResults.errorCount} failures out of ${results.length} servers`,
        "audit",
      );
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
  private logger: Logger;

  constructor(sshManagers: SSHManager | SSHManager[], projectName: string) {
    if (Array.isArray(sshManagers)) {
      this.auditAggregator = new AuditAggregator(sshManagers, projectName);
    } else {
      this.singleLogger = new RemoteAuditLogger(sshManagers, projectName);
    }
    this.logger = new Logger({ prefix: "audit" });
  }

  /**
   * Log deployment lock acquisition
   */
  async logLockAcquired(
    message: string,
    hosts: string[],
    acquiredBy?: string,
  ): Promise<{ host: string; success: boolean }[]> {
    const entry = {
      action: "deployment_lock",
      status: "success" as const,
      message: `Lock acquired: ${message}`,
      details: {
        message,
        hosts,
        acquiredBy: acquiredBy || "unknown",
        lockType: "exclusive",
      },
    };

    return await this.logEntryWithResults(entry);
  }

  /**
   * Log deployment lock release
   */
  async logLockReleased(
    hosts: string[],
    releasedBy?: string,
  ): Promise<{ host: string; success: boolean }[]> {
    const entry = {
      action: "deployment_unlock",
      status: "success" as const,
      message: `Lock released from ${hosts.length} host(s)`,
      details: {
        hosts,
        releasedBy: releasedBy || "unknown",
        lockType: "exclusive",
      },
    };

    return await this.logEntryWithResults(entry);
  }

  /**
   * Log lock acquisition failure
   */
  async logLockFailure(
    error: string,
    hosts: string[],
  ): Promise<{ host: string; success: boolean }[]> {
    const entry = {
      action: "deployment_lock",
      status: "failed" as const,
      message: `Failed to acquire lock: ${error}`,
      details: {
        error,
        hosts,
        lockType: "exclusive",
      },
    };

    return await this.logEntryWithResults(entry);
  }

  /**
   * Log a server initialization start event
   */
  async logInitStart(
    hosts: string[],
    engine: string,
  ): Promise<{ host: string; success: boolean }[]> {
    const entry = {
      action: "server_init",
      status: "started" as const,
      message:
        `Initialization started for ${hosts.length} host(s) with ${engine}`,
      details: {
        hosts,
        engine,
      },
    };

    return await this.logEntryWithResults(entry);
  }

  /**
   * Log a server initialization success event
   */
  async logInitSuccess(
    hosts: string[],
    engine: string,
  ): Promise<{ host: string; success: boolean }[]> {
    const entry = {
      action: "server_init",
      status: "success" as const,
      message:
        `Initialization completed successfully for ${hosts.length} host(s)`,
      details: {
        hosts,
        engine,
      },
    };

    return await this.logEntryWithResults(entry);
  }

  /**
   * Log a server initialization failure event
   */
  async logInitFailure(
    error: string,
    hosts?: string[],
    engine?: string,
  ): Promise<{ host: string; success: boolean }[]> {
    const entry = {
      action: "server_init",
      status: "failed" as const,
      message: `Initialization failed: ${error}`,
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
    version?: string,
  ): Promise<void> {
    const entry = {
      action: "service_deploy",
      status,
      message: message || `Service ${serviceName} deployment ${status}`,
      details: {
        serviceName,
        version,
        deploymentMethod: "rolling",
      },
    };

    await this.logEntry(entry);
  }

  /**
   * Log service rollback events
   */
  async logServiceRollback(
    serviceName: string,
    status: "started" | "success" | "failed",
    fromVersion?: string,
    toVersion?: string,
    message?: string,
  ): Promise<void> {
    const entry = {
      action: "service_rollback",
      status,
      message: message || `Service ${serviceName} rollback ${status}`,
      details: {
        serviceName,
        fromVersion,
        toVersion,
      },
    };

    await this.logEntry(entry);
  }

  /**
   * Log app events
   */
  async logAppEvent(
    action: "boot" | "stop" | "start" | "restart" | "remove",
    serviceName: string,
    status: "started" | "success" | "failed",
    message?: string,
  ): Promise<void> {
    const entry = {
      action: `app_${action}`,
      status,
      message: message || `App ${action} ${status} for ${serviceName}`,
      details: {
        serviceName,
        appAction: action,
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
    exitCode?: number,
  ): Promise<void> {
    const entry = {
      action: "custom_command",
      status,
      message: message || `Custom command ${status}: ${command}`,
      details: {
        command,
        exitCode,
      },
    };

    await this.logEntry(entry);
  }

  /**
   * Log container events
   */
  async logContainerEvent(
    action: "start" | "stop" | "remove" | "create",
    containerName: string,
    status: "started" | "success" | "failed",
    message?: string,
  ): Promise<void> {
    const entry = {
      action: `container_${action}`,
      status,
      message: message || `Container ${action} ${status}: ${containerName}`,
      details: {
        containerName,
        containerAction: action,
      },
    };

    await this.logEntry(entry);
  }

  /**
   * Log proxy events
   */
  async logProxyEvent(
    action: "boot" | "deploy" | "remove",
    status: "started" | "success" | "failed",
    message?: string,
  ): Promise<void> {
    const entry = {
      action: `proxy_${action}`,
      status,
      message: message || `Proxy ${action} ${status}`,
      details: {
        proxyAction: action,
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
        this.logger.warn(
          `Failed to log audit entry to ${failures.length} server(s):`,
        );
        failures.forEach((f) =>
          this.logger.warn(`   - ${f.host}: ${f.error || "Unknown error"}`)
        );
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
  projectName: string,
): ServerAuditLogger => {
  return new ServerAuditLogger(sshManagers, projectName);
};

/**
 * Local audit logger for operations that don't involve remote servers
 */
export class LocalAuditLogger {
  private auditDir: string;
  private auditFile: string;
  private projectName: string;
  private logger: Logger;

  constructor(projectName: string, projectRoot: string = Deno.cwd()) {
    this.projectName = projectName;
    this.auditDir = join(projectRoot, ".jiji", projectName);
    this.auditFile = join(this.auditDir, "audit.txt");
    this.logger = new Logger({ prefix: "audit" });
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
    } catch (error) {
      this.logger.warn(
        `Failed to write local audit entry: ${
          error instanceof Error ? error.message : String(error)
        }`,
      );
    }
  }

  /**
   * Format an audit entry for writing to file (enhanced for better readability)
   */
  private formatEntry(entry: AuditEntry): string {
    const timestamp = entry.timestamp;
    const status = entry.status.toUpperCase().padEnd(8);
    const action = entry.action.toUpperCase().replace(/_/g, " ");
    const host = entry.host ? ` [${entry.host}]` : "";
    const user = entry.user ? ` by ${entry.user}` : "";
    const message = entry.message || "";

    let line = `[${timestamp}] [${status}] ${action}${host}${user}`;
    if (message) {
      line += ` - ${message}`;
    }

    if (entry.details && Object.keys(entry.details).length > 0) {
      const importantDetails = this.extractImportantDetails(entry.details);
      if (importantDetails.length > 0) {
        line += `\n    ${importantDetails.join(", ")}`;
      }

      const detailsJson = JSON.stringify(entry.details, null, 0);
      if (detailsJson.length > 100) {
        line += `\n    Details: ${detailsJson}`;
      }
    }

    return line;
  }

  /**
   * Extract important details for display
   */
  private extractImportantDetails(details: Record<string, unknown>): string[] {
    const important: string[] = [];

    if (details.serviceName) important.push(`service=${details.serviceName}`);
    if (details.version) important.push(`version=${details.version}`);
    if (details.engine) important.push(`engine=${details.engine}`);
    if (details.command) important.push(`command="${details.command}"`);
    if (details.exitCode !== undefined) {
      important.push(`exit=${details.exitCode}`);
    }
    if (details.containerName) {
      important.push(`container=${details.containerName}`);
    }
    if (details.hosts && Array.isArray(details.hosts)) {
      important.push(`hosts=${details.hosts.length}`);
    }

    return important;
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
  projectName: string,
  projectRoot?: string,
): LocalAuditLogger => {
  return new LocalAuditLogger(projectName, projectRoot);
};
