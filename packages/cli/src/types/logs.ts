/**
 * Types for logs-related functionality
 */

/**
 * Common options for log commands
 */
export interface LogsOptions {
  since?: string;
  lines?: number;
  grep?: string;
  grepOptions?: string;
  follow?: boolean;
}

/**
 * Options for building Docker/Podman logs command
 */
export interface BuildLogsCommandOptions {
  lines?: number;
  grep?: string;
  grepOptions?: string;
  since?: string;
}

/**
 * Options for fetching container logs
 */
export interface FetchLogsOptions {
  lines?: number;
  grep?: string;
  grepOptions?: string;
  since?: string;
}

/**
 * Options for following container logs
 */
export interface FollowLogsOptions {
  lines?: number;
  grep?: string;
  grepOptions?: string;
  since?: string;
}
