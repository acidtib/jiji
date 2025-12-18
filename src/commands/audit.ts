import { Command } from "@cliffy/command";
import { colors } from "@cliffy/ansi/colors";

import { filterHostsByPatterns, loadConfig } from "../utils/config.ts";
import type { GlobalOptions } from "../types.ts";
import { setupSSHConnections, type SSHManager } from "../utils/ssh.ts";
import { createServerAuditLogger } from "../utils/audit.ts";
import { log, Logger } from "../utils/logger.ts";

interface AuditEntry {
  timestamp: string;
  status: string;
  action: string;
  host?: string;
  message: string;
  raw?: string;
}

export const auditCommand = new Command()
  .description("Show audit trail of deployments and operations")
  .option("-n, --lines <number:number>", "Number of recent entries to show", {
    default: 20,
  })
  .option(
    "--filter <action:string>",
    "Filter by action type (deploy, lock, bootstrap, etc.)",
  )
  .option(
    "--status <status:string>",
    "Filter by status (started|success|failed|warning)",
  )
  .option(
    "--since <date:string>",
    "Show entries since date (YYYY-MM-DD or ISO string)",
  )
  .option(
    "--until <date:string>",
    "Show entries until date (YYYY-MM-DD or ISO string)",
  )
  .option("--raw", "Show raw log format without formatting", {
    default: false,
  })
  .option("--json", "Output as JSON for programmatic use", {
    default: false,
  })
  .option("--follow", "Follow the audit log (like tail -f)", {
    default: false,
  })
  .option("--aggregate", "Combine logs from all servers chronologically", {
    default: true,
  })
  .action(async (options) => {
    let sshManagers: SSHManager[] | undefined;

    try {
      if (options.follow) {
        await followAuditLogs(options);
        return;
      }

      log.info("Jiji Audit Trail", "audit");

      // Load configuration
      const globalOptions = options as unknown as GlobalOptions;
      const { config, configPath } = await loadConfig(globalOptions.configFile);
      log.info(`Configuration: ${colors.dim(configPath)}`, "audit");

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
          console.error(
            `No hosts matched the pattern(s): ${globalOptions.hosts}`,
          );
          Deno.exit(1);
        }
      }

      if (targetHosts.length === 0) {
        log.warn("No remote hosts configured.", "audit");
        log.info(
          "Add hosts to your services in .jiji/deploy.yml to view remote audit trails.",
          "audit",
        );

        // Show local audit log if available
        await showLocalAuditLog(options);
        return;
      }

      log.info(`Hosts: ${colors.cyan(targetHosts.join(", "))}`, "audit");

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

      sshManagers = result.managers;
      const auditLogger = createServerAuditLogger(sshManagers, config.project);

      log.info("Fetching audit entries...", "audit");

      // Get audit entries from connected servers
      const serverLogs = await auditLogger.getRecentEntries(options.lines * 2);

      if (options.json) {
        await outputJsonFormat(serverLogs, options);
      } else if (options.aggregate) {
        await displayAggregatedEntries(serverLogs, options);
      } else {
        await displayServerEntries(serverLogs, options);
      }
    } catch (error) {
      log.error(`Failed to read audit trail:`, "audit");
      log.error(
        colors.red(error instanceof Error ? error.message : String(error)),
        "audit",
      );
      Deno.exit(1);
    } finally {
      // Always clean up SSH connections to prevent hanging
      if (sshManagers) {
        const cleanupLogger = new Logger({ prefix: "cleanup" });
        sshManagers.forEach((ssh) => {
          try {
            ssh.dispose();
          } catch (error) {
            // Ignore cleanup errors, but log them for debugging
            cleanupLogger.debug(`Failed to dispose SSH connection: ${error}`);
          }
        });
      }
    }
  });

/**
 * Show local audit log when no remote hosts are available
 */
async function showLocalAuditLog(options: {
  lines: number;
  filter?: string;
  status?: string;
  since?: string;
  until?: string;
  raw: boolean;
  json: boolean;
}) {
  try {
    const auditFile = ".jiji/audit.txt";
    const content = await Deno.readTextFile(auditFile);
    const lines = content.split("\n")
      .filter((line) => line.trim() && !line.startsWith("#"))
      .slice(-options.lines);

    if (lines.length === 0) {
      log.info("No local audit entries found.", "audit");
      return;
    }

    log.info("Local Audit Trail", "audit");

    for (const line of lines) {
      if (options.raw) {
        log.info(line, "audit");
      } else {
        log.info(formatAuditEntry(line), "audit");
      }
    }

    log.info(`${colors.dim(`${lines.length} local entries`)}`, "audit");
  } catch {
    log.info("No local audit file found.", "audit");
  }
}

/**
 * Follow audit logs in real-time
 */
async function followAuditLogs(
  _options: { hosts?: string[]; follow?: boolean },
) {
  log.info(
    colors.bold("Following Jiji Audit Trail (Press Ctrl+C to stop)\n"),
    "audit",
  );

  const seenEntries = new Set<string>();

  while (true) {
    try {
      // This is a simplified version - in practice you'd want to implement
      // proper log following with SSH connections
      await new Promise((resolve) => setTimeout(resolve, 2000));

      // Check local audit file for new entries
      try {
        const auditFile = ".jiji/audit.txt";
        const content = await Deno.readTextFile(auditFile);
        const lines = content.split("\n")
          .filter((line) => line.trim() && !line.startsWith("#"));

        for (const line of lines) {
          if (!seenEntries.has(line)) {
            seenEntries.add(line);
            log.info(formatAuditEntry(line), "audit");
          }
        }
      } catch {
        // Ignore file read errors
      }
    } catch (error) {
      if (error instanceof Error && error.name === "Interrupted") {
        break;
      }
      log.error(`Error following logs: ${error}`, "audit");
      await new Promise((resolve) => setTimeout(resolve, 5000));
    }
  }
}

/**
 * Output audit entries in JSON format
 */
function outputJsonFormat(
  serverLogs: { host: string; entries: string[] }[],
  options: {
    lines: number;
    filter?: string;
    status?: string;
    since?: string;
    until?: string;
  },
) {
  const allEntries: AuditEntry[] = [];

  for (const { host, entries } of serverLogs) {
    for (const entry of entries) {
      const parsed = parseAuditEntry(entry, host);
      if (parsed && shouldIncludeEntry(parsed, options)) {
        allEntries.push(parsed);
      }
    }
  }

  // Sort by timestamp
  allEntries.sort((a, b) =>
    new Date(a.timestamp).getTime() - new Date(b.timestamp).getTime()
  );

  log.info(
    JSON.stringify(
      {
        total: allEntries.length,
        entries: allEntries.slice(-options.lines),
      },
      null,
      2,
    ),
    "audit",
  );
}

/**
 * Display audit entries grouped by server
 */
function displayServerEntries(
  serverLogs: { host: string; entries: string[] }[],
  options: {
    lines: number;
    filter?: string;
    status?: string;
    since?: string;
    until?: string;
    raw: boolean;
  },
): void {
  let totalEntries = 0;

  for (const { host, entries } of serverLogs) {
    let filteredEntries = entries;

    // Apply filters
    filteredEntries = filterEntries(filteredEntries, options);

    // Take only the requested number after filtering
    filteredEntries = filteredEntries.slice(-options.lines);

    if (filteredEntries.length > 0) {
      log.info(`${colors.bold(colors.cyan(`Host: ${host}`))}`, "audit");
      log.info(`${"─".repeat(60)}`, "audit");

      for (const entry of filteredEntries) {
        if (options.raw) {
          log.info(entry, "audit");
        } else {
          log.info(formatAuditEntry(entry), "audit");
        }
      }

      log.info(
        colors.dim(`\n${filteredEntries.length} entries from ${host}\n`),
        "audit",
      );
      totalEntries += filteredEntries.length;
    }
  }

  if (totalEntries === 0) {
    log.info("No matching audit entries found.", "audit");
  } else {
    log.info(
      colors.dim(
        `Total: ${totalEntries} entries from ${serverLogs.length} server(s)`,
      ),
      "audit",
    );
  }
}

/**
 * Display audit entries aggregated chronologically
 */
function displayAggregatedEntries(
  serverLogs: { host: string; entries: string[] }[],
  options: {
    lines: number;
    filter?: string;
    status?: string;
    since?: string;
    until?: string;
    raw: boolean;
  },
): void {
  // Combine all entries with host information
  const allEntries: { entry: string; host: string; timestamp: Date }[] = [];

  for (const { host, entries } of serverLogs) {
    for (const entry of entries) {
      const parsed = parseAuditEntry(entry, host);
      if (parsed) {
        allEntries.push({
          entry: entry.includes(`[${host}]`)
            ? entry
            : entry.replace(/^(\[[^\]]+\]\s*\[[^\]]+\])/, `$1 [${host}]`),
          host,
          timestamp: new Date(parsed.timestamp),
        });
      }
    }
  }

  // Sort by timestamp
  allEntries.sort((a, b) => a.timestamp.getTime() - b.timestamp.getTime());

  // Apply filters
  let filteredEntries = allEntries.filter((item) =>
    shouldIncludeEntry(parseAuditEntry(item.entry, item.host), options)
  );

  // Take only the requested number after filtering
  filteredEntries = filteredEntries.slice(-options.lines);

  if (filteredEntries.length === 0) {
    log.info("No matching audit entries found.", "audit");
    return;
  }

  log.info(
    colors.bold(
      `Aggregated Audit Trail (${filteredEntries.length} entries)`,
    ),
    "audit",
  );
  log.info(
    colors.dim(
      `From ${serverLogs.length} server(s): ${
        serverLogs.map((s) => s.host).join(", ")
      }\n`,
    ),
    "audit",
  );

  for (const { entry } of filteredEntries) {
    if (options.raw) {
      log.info(entry, "audit");
    } else {
      log.info(formatAuditEntry(entry), "audit");
    }
  }

  log.info(colors.dim(`\nTotal: ${filteredEntries.length} entries`), "audit");
}

/**
 * Parse an audit entry string into structured data
 */
function parseAuditEntry(entry: string, host: string): AuditEntry | null {
  // Parse the entry format: [TIMESTAMP] [STATUS] ACTION [HOST] - MESSAGE
  const match = entry.match(
    /^\[([^\]]+)\]\s*\[([^\]]+)\]\s*([^\[\-]*?)(\[([^\]]+)\])?\s*-?\s*(.*)?$/,
  );

  if (!match) {
    return null;
  }

  const [, timestamp, status, action, , entryHost, message] = match;

  return {
    timestamp,
    status: status.trim().toLowerCase(),
    action: action.trim().toLowerCase(),
    host: entryHost || host,
    message: message?.trim() || "",
    raw: entry,
  };
}

/**
 * Check if an entry should be included based on filters
 */
function shouldIncludeEntry(entry: AuditEntry | null, options: {
  filter?: string;
  status?: string;
  since?: string;
  until?: string;
}): boolean {
  if (!entry) return false;

  if (options.filter && !entry.action.includes(options.filter.toLowerCase())) {
    return false;
  }

  if (options.status && entry.status !== options.status.toLowerCase()) {
    return false;
  }

  if (options.since) {
    const sinceDate = new Date(options.since);
    const entryDate = new Date(entry.timestamp);
    if (entryDate < sinceDate) {
      return false;
    }
  }

  if (options.until) {
    const untilDate = new Date(options.until);
    const entryDate = new Date(entry.timestamp);
    if (entryDate > untilDate) {
      return false;
    }
  }

  return true;
}

/**
 * Filter entries based on options
 */
function filterEntries(entries: string[], options: {
  filter?: string;
  status?: string;
  since?: string;
  until?: string;
}): string[] {
  return entries.filter((entry) => {
    if (
      options.filter &&
      !entry.toLowerCase().includes(options.filter.toLowerCase())
    ) {
      return false;
    }

    if (
      options.status && !entry.toLowerCase().includes(
        `[${options.status.toLowerCase().padEnd(8)}]`,
      )
    ) {
      return false;
    }

    // Add date filtering logic here if needed
    return true;
  });
}

/**
 * Colorize status text
 */
function colorizeStatus(status: string): string {
  const statusUpper = status.toUpperCase();

  switch (statusUpper) {
    case "SUCCESS":
      return colors.green(`[${statusUpper}]`);
    case "FAILED":
      return colors.red(`[${statusUpper}]`);
    case "WARNING":
      return colors.yellow(`[${statusUpper}]`);
    case "STARTED":
      return colors.blue(`[${statusUpper}]`);
    default:
      return colors.gray(`[${statusUpper}]`);
  }
}

/**
 * Format an audit entry for better readability
 */
function formatAuditEntry(entry: string): string {
  // Skip details lines (indented)
  if (entry.trim().startsWith("Details:")) {
    return colors.dim(`    ${entry.trim()}`);
  }

  // Parse the entry format: [TIMESTAMP] [STATUS] ACTION [HOST] - MESSAGE
  const match = entry.match(
    /^\[([^\]]+)\]\s*\[([^\]]+)\]\s*([^\[\-]*?)(\[([^\]]+)\])?\s*-?\s*(.*)?$/,
  );

  if (!match) {
    return entry; // Return as-is if format doesn't match
  }

  const [, timestamp, status, action, , host, message] = match;

  // Format timestamp
  const timeFormatted = colors.dim(
    new Date(timestamp).toLocaleString(),
  );

  // Color code based on status
  const statusFormatted = colorizeStatus(status.trim());

  // Format action
  const actionFormatted = colors.bold(action.trim().toUpperCase());

  // Format host
  const hostFormatted = host ? colors.cyan(`[${host}]`) : "";

  // Format message
  const messageFormatted = message ? colors.white(message.trim()) : "";

  return `${timeFormatted} ${statusFormatted} ${actionFormatted}${
    hostFormatted ? " " + hostFormatted : ""
  }${messageFormatted ? " - " + messageFormatted : ""}`;
}
