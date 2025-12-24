import { Command } from "@cliffy/command";
import { colors } from "@cliffy/ansi/colors";
import {
  cleanupSSHConnections,
  setupCommandContext,
} from "../utils/command_helpers.ts";
import { handleCommandError } from "../utils/error_handler.ts";
import type { GlobalOptions } from "../types.ts";
import { createServerAuditLogger } from "../utils/audit.ts";
import { log } from "../utils/logger.ts";

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
    "Filter by action type (deploy, lock, init, etc.)",
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
    const globalOptions = options as unknown as GlobalOptions;
    let ctx: Awaited<ReturnType<typeof setupCommandContext>> | undefined;

    try {
      if (options.follow) {
        await followAuditLogs(options);
        return;
      }

      log.section("Audit Trail:");

      try {
        ctx = await setupCommandContext(globalOptions);
      } catch (_error) {
        log.warn("\nNo remote hosts configured");
        log.say(
          "Add hosts to your services in .jiji/deploy.yml to view remote audit trails.",
          1,
        );
        await showLocalAuditLog(options);
        return;
      }

      const { config, sshManagers, targetHosts } = ctx;

      log.say(
        `Configuration loaded from: ${config.configPath || "unknown"}`,
        1,
      );
      log.say(`Hosts: ${targetHosts.join(", ")}`, 1);

      console.log("");
      for (const ssh of sshManagers) {
        log.remote(ssh.getHost(), ": Connected", { indent: 1 });
      }

      const auditLogger = createServerAuditLogger(sshManagers, config.project);

      log.section("Fetching Entries:");
      log.say(`- Retrieving up to ${options.lines * 2} recent entries`, 1);

      const serverLogs = await auditLogger.getRecentEntries(options.lines * 2);

      if (options.json) {
        outputJsonFormat(serverLogs, options);
      } else if (options.aggregate) {
        displayAggregatedEntries(serverLogs, options);
      } else {
        displayServerEntries(serverLogs, options);
      }
    } catch (error) {
      await handleCommandError(error, {
        operation: "Audit",
        component: "audit",
        sshManagers: ctx?.sshManagers,
        projectName: ctx?.config?.project,
        targetHosts: ctx?.targetHosts,
      });
    } finally {
      if (ctx?.sshManagers) {
        cleanupSSHConnections(ctx.sshManagers);
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

    log.section("Local Audit Trail");

    for (const line of lines) {
      if (options.raw) {
        log.say(line);
      } else {
        log.say(formatAuditEntry(line));
      }
    }

    console.log();
    log.say(`${colors.dim(`${lines.length} local entries`)}`);
  } catch {
    log.say("No local audit file found");
  }
}

/**
 * Follow audit logs in real-time
 */
async function followAuditLogs(
  _options: { hosts?: string[]; follow?: boolean },
) {
  log.section("Following Jiji Audit Trail");
  log.say(colors.dim("Press Ctrl+C to stop"));
  console.log();

  const seenEntries = new Set<string>();

  while (true) {
    try {
      await new Promise((resolve) => setTimeout(resolve, 2000));

      try {
        const auditFile = ".jiji/audit.txt";
        const content = await Deno.readTextFile(auditFile);
        const lines = content.split("\n")
          .filter((line) => line.trim() && !line.startsWith("#"));

        for (const line of lines) {
          if (!seenEntries.has(line)) {
            seenEntries.add(line);
            log.say(formatAuditEntry(line));
          }
        }
      } catch {
        // Ignore file read errors
      }
    } catch (error) {
      if (error instanceof Error && error.name === "Interrupted") {
        break;
      }
      log.error(`Error following logs: ${error}`);
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

  allEntries.sort((a, b) =>
    new Date(a.timestamp).getTime() - new Date(b.timestamp).getTime()
  );

  log.say(
    JSON.stringify(
      {
        total: allEntries.length,
        entries: allEntries.slice(-options.lines),
      },
      null,
      2,
    ),
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

    filteredEntries = filterEntries(filteredEntries, options);
    filteredEntries = filteredEntries.slice(-options.lines);

    if (filteredEntries.length > 0) {
      log.section(`Host: ${host}`);

      for (const entry of filteredEntries) {
        if (options.raw) {
          log.say(entry);
        } else {
          log.say(formatAuditEntry(entry));
        }
      }

      console.log();
      log.say(
        colors.dim(`${filteredEntries.length} entries from ${host}`),
      );
      console.log();
      totalEntries += filteredEntries.length;
    }
  }

  if (totalEntries === 0) {
    log.say("No matching audit entries found");
  } else {
    log.say(
      colors.dim(
        `Total: ${totalEntries} entries from ${serverLogs.length} server(s)`,
      ),
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

  allEntries.sort((a, b) => a.timestamp.getTime() - b.timestamp.getTime());

  let filteredEntries = allEntries.filter((item) =>
    shouldIncludeEntry(parseAuditEntry(item.entry, item.host), options)
  );

  filteredEntries = filteredEntries.slice(-options.lines);

  if (filteredEntries.length === 0) {
    log.say("No matching audit entries found");
    return;
  }

  log.section(`Aggregated Audit Trail (${filteredEntries.length} entries)`);
  log.say(
    colors.dim(
      `From ${serverLogs.length} server(s): ${
        serverLogs.map((s) => s.host).join(", ")
      }`,
    ),
    1,
  );

  console.log();

  for (const { entry } of filteredEntries) {
    if (options.raw) {
      log.say(entry);
    } else {
      log.say(formatAuditEntry(entry));
    }
  }

  console.log();
  log.say(colors.dim(`Total: ${filteredEntries.length} entries`));
}

/**
 * Parse an audit entry string into structured data
 */
function parseAuditEntry(entry: string, host: string): AuditEntry | null {
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
  if (entry.trim().startsWith("Details:")) {
    return colors.dim(`    ${entry.trim()}`);
  }

  const match = entry.match(
    /^\[([^\]]+)\]\s*\[([^\]]+)\]\s*([^\[\-]*?)(\[([^\]]+)\])?\s*-?\s*(.*)?$/,
  );

  if (!match) {
    return entry;
  }

  const [, timestamp, status, action, , host, message] = match;

  const timeFormatted = colors.dim(
    new Date(timestamp).toLocaleString(),
  );

  const statusFormatted = colorizeStatus(status.trim());
  const actionFormatted = colors.bold(action.trim().toUpperCase());
  const hostFormatted = host ? colors.cyan(`[${host}]`) : "";
  const messageFormatted = message ? colors.white(message.trim()) : "";

  return `${timeFormatted} ${statusFormatted} ${actionFormatted}${
    hostFormatted ? " " + hostFormatted : ""
  }${messageFormatted ? " - " + messageFormatted : ""}`;
}
