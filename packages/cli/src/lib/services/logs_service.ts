/**
 * Service for fetching and following container logs
 */

import { log } from "../../utils/logger.ts";
import type { SSHManager } from "../../utils/ssh.ts";
import type {
  BuildLogsCommandOptions,
  FetchLogsOptions,
  FollowLogsOptions,
} from "../../types.ts";

/**
 * Logs service for managing container log operations
 */
export class LogsService {
  constructor(
    private engine: string,
    private component: string = "logs",
  ) {}

  /**
   * Build the Docker/Podman logs command
   */
  buildLogsCommand(
    containerNameOrId: string,
    options: BuildLogsCommandOptions,
    follow: boolean,
  ): string {
    const parts: string[] = [this.engine, "logs"];

    // Add timestamps
    parts.push("--timestamps");

    // Add follow flag
    if (follow) {
      parts.push("--follow");
    }

    // Add since option
    if (options.since) {
      parts.push(`--since="${options.since}"`);
    }

    // Add tail/lines option
    if (options.lines !== undefined) {
      parts.push(`--tail=${options.lines}`);
    }

    // Add container name/ID
    parts.push(containerNameOrId);

    // Add grep if specified
    if (options.grep) {
      const grepOpts = options.grepOptions || "";
      parts.push(`| grep ${grepOpts} "${options.grep}"`);
    }

    return parts.join(" ");
  }

  /**
   * Follow logs for a specific container using SSH interactive session
   */
  async followContainerLogs(
    ssh: SSHManager,
    containerNameOrId: string,
    logOptions: FollowLogsOptions,
  ): Promise<void> {
    const logsCmd = this.buildLogsCommand(
      containerNameOrId,
      logOptions,
      true, // follow mode
    );

    try {
      // Use interactive session for following logs
      await ssh.startInteractiveSession(logsCmd);
    } catch (error) {
      if (
        error instanceof Error && error.message.includes("No such container")
      ) {
        log.error(
          `Container ${containerNameOrId} not found`,
          this.component,
        );
      } else {
        throw error;
      }
    }
  }

  /**
   * Fetch logs for a specific container
   */
  async fetchContainerLogs(
    ssh: SSHManager,
    host: string,
    containerNameOrId: string,
    logOptions: FetchLogsOptions,
  ): Promise<void> {
    const logsCmd = this.buildLogsCommand(
      containerNameOrId,
      logOptions,
      false, // not following
    );

    try {
      const result = await ssh.executeCommand(logsCmd);

      if (!result.success) {
        if (
          result.stderr.includes("No such container") ||
          result.stderr.includes("No such object")
        ) {
          log.hostOutput(host, `Container ${containerNameOrId} not found`, {
            type: "Logs",
          });
        } else {
          log.hostOutput(
            host,
            `Failed to fetch logs: ${result.stderr}`,
            { type: "Error" },
          );
        }
        return;
      }

      const output = result.stdout.trim();
      if (output) {
        // Use host-grouped output pattern
        log.hostOutput(host, output, { type: "Logs" });
      } else {
        log.hostOutput(host, "No logs found", { type: "Logs" });
      }
    } catch (error) {
      log.error(
        `Error fetching logs from ${containerNameOrId} on ${host}: ${
          error instanceof Error ? error.message : String(error)
        }`,
        this.component,
      );
    }
  }
}
