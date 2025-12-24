/**
 * Command to show logs from kamal-proxy
 */

import { Command } from "@cliffy/command";
import {
  cleanupSSHConnections,
  displayCommandHeader,
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
      // Setup command context (load config and establish SSH connections)
      ctx = await setupCommandContext(globalOptions);
      const context = ctx;
      const { config } = context;

      // Display standardized command header
      displayCommandHeader("Proxy Logs:", config, context.sshManagers);

      if (context.targetHosts.length === 0) {
        log.error(
          "No servers are reachable. Cannot fetch proxy logs.",
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

  log.section("Following Logs:");
  log.say(`- Host: ${primaryHost}`, 1);
  log.say(`- Container: ${KAMAL_PROXY_CONTAINER_NAME}`, 1);

  const ssh = context.sshManagers.find((ssh) => ssh.getHost() === primaryHost);
  if (!ssh) {
    log.error(`SSH connection not found for host ${primaryHost}`);
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
  log.section("Fetching Logs:");
  log.say(`- Container: ${KAMAL_PROXY_CONTAINER_NAME}`, 1);

  // Fetch logs from each host
  for (const host of context.targetHosts) {
    const ssh = context.sshManagers.find((ssh) => ssh.getHost() === host);
    if (!ssh) {
      log.warn(`SSH connection not found for host ${host}`);
      continue;
    }

    await logsService.fetchContainerLogs(
      ssh,
      host,
      KAMAL_PROXY_CONTAINER_NAME,
      logOptions,
    );
  }
}
