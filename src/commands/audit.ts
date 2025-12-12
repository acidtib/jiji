import { Command } from "@cliffy/command";
import { colors } from "@cliffy/ansi/colors";
import { loadConfig } from "../utils/config.ts";
import {
  createSSHConfigFromJiji,
  createSSHManagers,
  filterConnectedHosts,
  testConnections,
  validateSSHSetup,
} from "../utils/ssh.ts";
import { createServerAuditLogger } from "../utils/audit.ts";

export const auditCommand = new Command()
  .description("View audit trail from remote servers")
  .option("-n, --lines <number:number>", "Number of recent entries to show", {
    default: 20,
  })
  .option("-c, --config <path:string>", "Path to jiji.yml config file")
  .option("--ssh-user <username:string>", "SSH username for remote hosts")
  .option("--ssh-port <port:number>", "SSH port (default: 22)")
  .option("--filter <action:string>", "Filter by action type")
  .option(
    "--status <status:string>",
    "Filter by status (started|success|failed|warning)",
  )
  .option("--host <hostname:string>", "Filter by specific host")
  .option("--raw", "Show raw log format without formatting", {
    default: false,
  })
  .option("--aggregate", "Combine logs from all servers chronologically", {
    default: false,
  })
  .action(async (options) => {
    let sshManagers: ReturnType<typeof createSSHManagers> | undefined;

    try {
      console.log("📋 Loading audit trail from remote servers...\n");

      // Load configuration
      const { config, configPath } = await loadConfig(options.config);
      console.log(`Configuration loaded from: ${configPath}`);

      // Collect all unique hosts from services
      const allHosts = new Set<string>();
      for (const service of Object.values(config.services)) {
        if (service.hosts) {
          service.hosts.forEach((host: string) => allHosts.add(host));
        }
      }

      let targetHosts = Array.from(allHosts);

      // Filter by specific host if requested
      if (options.host) {
        targetHosts = targetHosts.filter((host) => host === options.host);
        if (targetHosts.length === 0) {
          console.error(`❌ Host '${options.host}' not found in configuration`);
          Deno.exit(1);
        }
      }

      if (targetHosts.length === 0) {
        console.log("📝 No remote hosts found in configuration.");
        console.log(
          "💡 Add hosts to your services in .jiji/deploy.yml to view remote audit trails.",
        );
        return;
      }

      console.log(
        `Connecting to ${targetHosts.length} host(s): ${
          targetHosts.join(", ")
        }`,
      );

      // Validate SSH setup
      const sshValidation = await validateSSHSetup();
      if (!sshValidation.valid) {
        console.error(`❌ SSH setup validation failed:`);
        console.error(`   ${sshValidation.message}`);
        console.error(
          `   Please run 'ssh-agent' and 'ssh-add' before continuing.`,
        );
        Deno.exit(1);
      }

      // Get SSH configuration
      const baseSshConfig = createSSHConfigFromJiji(config.ssh);
      const sshConfig = {
        username: options.sshUser || baseSshConfig.username,
        port: options.sshPort || baseSshConfig.port,
        useAgent: true,
      };

      // Create SSH managers for target hosts and test connections
      sshManagers = createSSHManagers(targetHosts, sshConfig);
      const connectionTests = await testConnections(sshManagers);

      const { connectedManagers, connectedHosts, failedHosts } =
        filterConnectedHosts(sshManagers, connectionTests);

      if (connectedHosts.length === 0) {
        console.error(
          "❌ No hosts are reachable. Cannot fetch audit entries.",
        );
        Deno.exit(1);
      }

      if (failedHosts.length > 0) {
        console.log(
          `\n⚠️  Skipping unreachable hosts: ${failedHosts.join(", ")}`,
        );
        console.log(
          `Proceeding with ${connectedHosts.length} reachable host(s): ${
            connectedHosts.join(", ")
          }\n`,
        );
      }

      // Use only connected SSH managers
      sshManagers = connectedManagers;
      const auditLogger = createServerAuditLogger(sshManagers);

      console.log("Fetching audit entries...\n");

      // Get audit entries from connected servers
      const serverLogs = await auditLogger.getRecentEntries(options.lines * 2);

      if (options.aggregate) {
        await displayAggregatedEntries(serverLogs, options);
      } else {
        await displayServerEntries(serverLogs, options);
      }
    } catch (error) {
      console.error("❌ Failed to read audit trail:");
      console.error(error instanceof Error ? error.message : String(error));
      Deno.exit(1);
    } finally {
      // Always clean up SSH connections to prevent hanging
      if (sshManagers) {
        sshManagers.forEach((ssh) => {
          try {
            ssh.dispose();
          } catch (error) {
            // Ignore cleanup errors, but log them for debugging
            console.debug(`Failed to dispose SSH connection: ${error}`);
          }
        });
      }
    }
  });

/**
 * Display audit entries grouped by server
 */
function displayServerEntries(
  serverLogs: { host: string; entries: string[] }[],
  options: {
    lines: number;
    filter?: string;
    status?: string;
    raw: boolean;
  },
): void {
  let totalEntries = 0;

  for (const { host, entries } of serverLogs) {
    let filteredEntries = entries;

    // Apply filters
    if (options.filter) {
      filteredEntries = filteredEntries.filter((entry) =>
        entry.toLowerCase().includes(options.filter!.toLowerCase())
      );
    }

    if (options.status) {
      filteredEntries = filteredEntries.filter((entry) =>
        entry.toLowerCase().includes(
          `[${options.status!.toLowerCase().padEnd(8)}]`,
        )
      );
    }

    // Take only the requested number after filtering
    filteredEntries = filteredEntries.slice(-options.lines);

    if (filteredEntries.length > 0) {
      console.log(`${colors.bold(colors.cyan(`📍 ${host}`))}`);
      console.log(`${"─".repeat(50)}`);

      for (const entry of filteredEntries) {
        if (options.raw) {
          console.log(entry);
        } else {
          console.log(formatAuditEntry(entry));
        }
      }

      console.log(`\n📊 ${filteredEntries.length} entries from ${host}\n`);
      totalEntries += filteredEntries.length;
    }
  }

  if (totalEntries === 0) {
    console.log("📝 No matching audit entries found.");
  } else {
    console.log(
      `Total: ${totalEntries} entries from ${serverLogs.length} server(s)`,
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
    raw: boolean;
  },
): void {
  // Combine all entries with host information
  const allEntries: { entry: string; host: string; timestamp: string }[] = [];

  for (const { host, entries } of serverLogs) {
    for (const entry of entries) {
      // Extract timestamp from entry for sorting
      const timestampMatch = entry.match(/\[([^\]]+)\]/);
      const timestamp = timestampMatch
        ? timestampMatch[1]
        : new Date().toISOString();

      allEntries.push({
        entry: entry.includes(`[${host}]`)
          ? entry
          : entry.replace(/^(\[[^\]]+\]\s*\[[^\]]+\])/, `$1 [${host}]`),
        host,
        timestamp,
      });
    }
  }

  // Sort by timestamp
  allEntries.sort((a, b) =>
    new Date(a.timestamp).getTime() - new Date(b.timestamp).getTime()
  );

  let filteredEntries = allEntries;

  // Apply filters
  if (options.filter) {
    filteredEntries = filteredEntries.filter((item) =>
      item.entry.toLowerCase().includes(options.filter!.toLowerCase())
    );
  }

  if (options.status) {
    filteredEntries = filteredEntries.filter((item) =>
      item.entry.toLowerCase().includes(
        `[${options.status!.toLowerCase().padEnd(8)}]`,
      )
    );
  }

  // Take only the requested number after filtering
  filteredEntries = filteredEntries.slice(-options.lines);

  if (filteredEntries.length === 0) {
    console.log("📝 No matching audit entries found.");
    return;
  }

  console.log(`Aggregated Audit Trail (${filteredEntries.length} entries)`);
  console.log(
    `From ${serverLogs.length} server(s): ${
      serverLogs.map((s) => s.host).join(", ")
    }\n`,
  );

  for (const { entry } of filteredEntries) {
    if (options.raw) {
      console.log(entry);
    } else {
      console.log(formatAuditEntry(entry));
    }
  }

  console.log(`\nTotal: ${filteredEntries.length} entries`);
}

/**
 * Format an audit entry for better readability
 */
function formatAuditEntry(entry: string): string {
  // Skip details lines (indented)
  if (entry.trim().startsWith("Details:")) {
    return colors.gray(`    ${entry.trim()}`);
  }

  // Parse the entry format: [TIMESTAMP] [STATUS] ACTION [HOST] - MESSAGE
  const match = entry.match(
    /^\[([^\]]+)\]\s*\[([^\]]+)\]\s*([^\[\-]*?)(\[([^\]]+)\])?\s*-?\s*(.*)?$/,
  );

  if (!match) {
    return entry; // Return as-is if format doesn't match
  }

  const [, timestamp, status, action, , host, message] = match;

  // Color code based on status
  let statusColor = colors.gray;
  const statusUpper = status.trim().toUpperCase();

  switch (statusUpper) {
    case "SUCCESS":
      statusColor = colors.green;
      break;
    case "FAILED":
      statusColor = colors.red;
      break;
    case "WARNING":
      statusColor = colors.yellow;
      break;
    case "STARTED":
      statusColor = colors.blue;
      break;
  }

  // Build formatted output
  const timeFormatted = colors.gray(new Date(timestamp).toLocaleString());
  const statusFormatted = statusColor(`[${statusUpper.padEnd(7)}]`);
  const actionFormatted = colors.bold(action.trim().toUpperCase());
  const hostFormatted = host ? colors.cyan(`[${host}]`) : "";
  const messageFormatted = message ? colors.white(message.trim()) : "";

  return `${timeFormatted} ${statusFormatted} ${actionFormatted}${
    hostFormatted ? " " + hostFormatted : ""
  }${messageFormatted ? " - " + messageFormatted : ""}`;
}
