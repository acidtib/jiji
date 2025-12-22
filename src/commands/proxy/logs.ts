/**
 * Command to show logs from kamal-proxy
 */

import { Command } from "@cliffy/command";
import {
  cleanupSSHConnections,
  setupCommandContext,
} from "../../utils/command_helpers.ts";
import { handleCommandError } from "../../utils/error_handler.ts";
import { log } from "../../utils/logger.ts";
import { LogsService } from "../../lib/services/logs_service.ts";
import { KAMAL_PROXY_CONTAINER_NAME } from "../../constants.ts";

import type { GlobalOptions, LogsOptions } from "../../types.ts";

export const logsCommand = new Command()
  .description("Show log lines from kamal-proxy on servers")
  .option(
    "-s, --since=<since:string>",
    "Show logs since timestamp (e.g. 2013-01-02T13:23:37Z) or relative (e.g. 42m for 42 minutes)",
  )
  .option(
    "-n, --lines=<lines:number>",
    "Number of log lines to pull from each server",
  )
  .option(
    "-g, --grep=<grep:string>",
    "Show lines with grep match only (use this to fetch specific requests by id)",
  )
  .option(
    "-f, --follow",
    "Follow logs on primary server (or specific host set by --hosts)",
  )
  .action(async (options) => {
    const logsOptions = options as unknown as LogsOptions;
    const globalOptions = options as unknown as GlobalOptions;
    let ctx: Awaited<ReturnType<typeof setupCommandContext>> | undefined;

    try {
      // Setup command context
      ctx = await setupCommandContext(globalOptions);
      const context = ctx;

      if (context.targetHosts.length === 0) {
        log.error(
          "No servers are reachable. Cannot fetch proxy logs.",
          "proxy-logs",
        );
        Deno.exit(1);
      }

      // Create logs service
      const logsService = new LogsService(
        context.config.builder.engine,
        "proxy-logs",
      );

      // Build logs command options
      const grep = logsOptions.grep;
      const since = logsOptions.since;

      // Default lines: 100 if no since/grep, otherwise unlimited
      const lines = logsOptions.lines ??
        ((since || grep) ? undefined : 100);

      if (logsOptions.follow) {
        // Follow mode - only on primary host
        await followProxyLogs(
          context,
          logsService,
          { lines, grep, since },
        );
      } else {
        // Standard mode - fetch from all hosts
        await fetchProxyLogs(
          context,
          logsService,
          { lines, grep, since },
        );
      }

      // Close SSH connections
      cleanupSSHConnections(context.sshManagers);
    } catch (error) {
      if (ctx) {
        await handleCommandError(error, {
          operation: "Proxy Logs",
          component: "proxy-logs",
          sshManagers: ctx.sshManagers,
          projectName: ctx.config.project,
          targetHosts: ctx.targetHosts,
        });
      } else {
        log.error(
          `Proxy logs command failed: ${
            error instanceof Error ? error.message : String(error)
          }`,
          "proxy-logs",
        );
      }
      Deno.exit(1);
    }
  });

/**
 * Follow proxy logs from the primary server
 */
async function followProxyLogs(
  context: Awaited<ReturnType<typeof setupCommandContext>>,
  logsService: LogsService,
  logOptions: {
    lines?: number;
    grep?: string;
    since?: string;
  },
): Promise<void> {
  const primaryHost = context.targetHosts[0];
  log.info(
    `Following logs from ${KAMAL_PROXY_CONTAINER_NAME} on ${primaryHost}...`,
    "proxy-logs",
  );

  const ssh = context.sshManagers.find((ssh) => ssh.getHost() === primaryHost);
  if (!ssh) {
    log.error(`SSH connection not found for host ${primaryHost}`, "proxy-logs");
    Deno.exit(1);
  }

  await logsService.followContainerLogs(
    ssh,
    KAMAL_PROXY_CONTAINER_NAME,
    logOptions,
  );
}

/**
 * Fetch proxy logs from all hosts
 */
async function fetchProxyLogs(
  context: Awaited<ReturnType<typeof setupCommandContext>>,
  logsService: LogsService,
  logOptions: {
    lines?: number;
    grep?: string;
    since?: string;
  },
): Promise<void> {
  await log.group("Proxy Logs", async () => {
    log.info(
      `Fetching logs from ${KAMAL_PROXY_CONTAINER_NAME}...`,
      "proxy-logs",
    );

    // Fetch logs from each host
    for (const host of context.targetHosts) {
      const ssh = context.sshManagers.find((ssh) => ssh.getHost() === host);
      if (!ssh) {
        log.warn(`SSH connection not found for host ${host}`, "proxy-logs");
        continue;
      }

      log.info(
        `Logs from ${KAMAL_PROXY_CONTAINER_NAME} on ${host}:`,
        "proxy-logs",
      );

      await logsService.fetchContainerLogs(
        ssh,
        host,
        KAMAL_PROXY_CONTAINER_NAME,
        logOptions,
      );
    }
  });
}
