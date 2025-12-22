/**
 * Command to show logs from services
 */

import { Command } from "@cliffy/command";
import {
  cleanupSSHConnections,
  setupCommandContext,
} from "../../utils/command_helpers.ts";
import { handleCommandError } from "../../utils/error_handler.ts";
import { log } from "../../utils/logger.ts";
import { LogsService } from "../../lib/services/logs_service.ts";

import type { GlobalOptions, LogsOptions } from "../../types.ts";
import type { ServiceConfiguration } from "../../lib/configuration/service.ts";

interface ServiceLogsOptions extends LogsOptions {
  containerId?: string;
}

export const logsCommand = new Command()
  .description("Show log lines from services on servers")
  .option(
    "-s, --since=<since:string>",
    "Show lines since timestamp (e.g. 2013-01-02T13:23:37Z) or relative (e.g. 42m for 42 minutes)",
  )
  .option(
    "-n, --lines=<lines:number>",
    "Number of lines to show from each server",
  )
  .option(
    "-g, --grep=<grep:string>",
    "Show lines with grep match only (use this to fetch specific requests by id)",
  )
  .option(
    "--grep-options=<grepOptions:string>",
    "Additional options supplied to grep",
  )
  .option(
    "-f, --follow",
    "Follow log on primary server (or specific host set by --hosts)",
  )
  .option(
    "--container-id=<containerId:string>",
    "Container ID to fetch logs (to fetch logs from services running on a host but not part of our services)",
  )
  .action(async (options) => {
    const logsOptions = options as unknown as ServiceLogsOptions;
    const globalOptions = options as unknown as GlobalOptions;
    let ctx: Awaited<ReturnType<typeof setupCommandContext>> | undefined;

    try {
      // Validate that services are specified
      if (!globalOptions.services && !logsOptions.containerId) {
        log.error(
          "You must specify --services (-S) to target specific services or --container-id for a specific container.",
          "logs",
        );
        log.info(
          "Example: jiji services logs --services web,api",
          "logs",
        );
        Deno.exit(1);
      }

      // Setup command context
      ctx = await setupCommandContext(globalOptions);
      const context = ctx;

      if (context.targetHosts.length === 0) {
        log.error(
          "No servers are reachable. Cannot fetch logs.",
          "logs",
        );
        Deno.exit(1);
      }

      // Create logs service
      const logsService = new LogsService(
        context.config.builder.engine,
        "logs",
      );

      // Build logs command options
      const grep = logsOptions.grep;
      const grepOptions = logsOptions.grepOptions;
      const since = logsOptions.since;
      const containerId = logsOptions.containerId;

      // Default lines: 100 if no since/grep, otherwise unlimited
      const lines = logsOptions.lines ??
        ((since || grep) ? undefined : 100);

      if (logsOptions.follow) {
        // Follow mode - only on primary host
        await followLogs(
          context,
          logsService,
          { lines, grep, grepOptions, since, containerId },
        );
      } else {
        // Standard mode - fetch from all hosts
        await fetchLogs(
          context,
          logsService,
          { lines, grep, grepOptions, since, containerId },
        );
      }

      // Close SSH connections
      cleanupSSHConnections(context.sshManagers);
    } catch (error) {
      if (ctx) {
        await handleCommandError(error, {
          operation: "Service Logs",
          component: "logs",
          sshManagers: ctx.sshManagers,
          projectName: ctx.config.project,
          targetHosts: ctx.targetHosts,
        });
      } else {
        log.error(
          `Logs command failed: ${
            error instanceof Error ? error.message : String(error)
          }`,
          "logs",
        );
      }
      Deno.exit(1);
    }
  });

/**
 * Follow logs from the primary server
 */
async function followLogs(
  context: Awaited<ReturnType<typeof setupCommandContext>>,
  logsService: LogsService,
  logOptions: {
    lines?: number;
    grep?: string;
    grepOptions?: string;
    since?: string;
    containerId?: string;
  },
): Promise<void> {
  const primaryHost = context.targetHosts[0];
  log.info(`Following logs on ${primaryHost}...`, "logs");

  // If container ID is specified, use it directly
  if (logOptions.containerId) {
    const ssh = context.sshManagers.find((ssh) =>
      ssh.getHost() === primaryHost
    );
    if (!ssh) {
      log.error(`SSH connection not found for host ${primaryHost}`, "logs");
      Deno.exit(1);
    }

    await logsService.followContainerLogs(
      ssh,
      logOptions.containerId,
      logOptions,
    );
    return;
  }

  // Otherwise, follow logs for the first matching service on primary host
  let servicesToShow = Array.from(context.config.services.values());

  if (context.matchingServices && context.matchingServices.length > 0) {
    servicesToShow = servicesToShow.filter((service: ServiceConfiguration) =>
      context.matchingServices!.includes(service.name)
    );
  }

  if (servicesToShow.length === 0) {
    log.error("No services found to show logs.", "logs");
    Deno.exit(1);
  }

  // Use the first service
  const service = servicesToShow[0];
  const ssh = context.sshManagers.find((ssh) => ssh.getHost() === primaryHost);

  if (!ssh) {
    log.error(`SSH connection not found for host ${primaryHost}`, "logs");
    Deno.exit(1);
  }

  const containerName = service.getContainerName();
  log.info(`Following logs for ${containerName}...`, "logs");

  await logsService.followContainerLogs(
    ssh,
    containerName,
    logOptions,
  );
}

/**
 * Fetch logs from all hosts
 */
async function fetchLogs(
  context: Awaited<ReturnType<typeof setupCommandContext>>,
  logsService: LogsService,
  logOptions: {
    lines?: number;
    grep?: string;
    grepOptions?: string;
    since?: string;
    containerId?: string;
  },
): Promise<void> {
  await log.group("Service Logs", async () => {
    // If container ID is specified, use it directly
    if (logOptions.containerId) {
      for (const host of context.targetHosts) {
        const ssh = context.sshManagers.find((ssh) => ssh.getHost() === host);
        if (!ssh) {
          log.warn(`SSH connection not found for host ${host}`, "logs");
          continue;
        }

        log.info(`Logs from ${host}:`, "logs");
        await logsService.fetchContainerLogs(
          ssh,
          host,
          logOptions.containerId,
          logOptions,
        );
      }
      return;
    }

    // Get services to show logs for
    let servicesToShow = Array.from(context.config.services.values());

    if (context.matchingServices && context.matchingServices.length > 0) {
      servicesToShow = servicesToShow.filter(
        (service: ServiceConfiguration) =>
          context.matchingServices!.includes(service.name),
      );
    }

    if (servicesToShow.length === 0) {
      log.error("No services found to show logs.", "logs");
      Deno.exit(1);
    }

    log.info(
      `Fetching logs for: ${
        servicesToShow.map((s: ServiceConfiguration) => s.name).join(", ")
      }`,
      "logs",
    );

    // Fetch logs for each service on each host
    for (const service of servicesToShow) {
      const serviceHosts = service.servers
        .map((server: { host: string }) => server.host)
        .filter((host: string) => context.targetHosts.includes(host));

      if (serviceHosts.length === 0) {
        log.warn(`No target hosts found for service ${service.name}`, "logs");
        continue;
      }

      for (const host of serviceHosts) {
        const ssh = context.sshManagers.find((ssh) => ssh.getHost() === host);
        if (!ssh) {
          log.warn(`SSH connection not found for host ${host}`, "logs");
          continue;
        }

        const containerName = service.getContainerName();
        log.info(`Logs from ${service.name} on ${host}:`, "logs");

        await logsService.fetchContainerLogs(
          ssh,
          host,
          containerName,
          logOptions,
        );
      }
    }
  });
}
