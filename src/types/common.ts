/**
 * Common utility types used across the application
 */

/**
 * Log levels for structured logging
 */
export type LogLevel =
  | "debug"
  | "info"
  | "warn"
  | "error"
  | "fatal"
  | "success"
  | "trace";

/**
 * Logger configuration options
 */
export interface LoggerOptions {
  prefix?: string;
  showTimestamp?: boolean;
  maxPrefixLength?: number;
  colors?: boolean;
  minLevel?: LogLevel;
  quiet?: boolean;
}

/**
 * Result of an operation that can succeed or fail
 */
export interface OperationResult<T> {
  success: boolean;
  result?: T;
  error?: Error;
  host?: string;
}

/**
 * Aggregated results containing both successes and failures
 */
export interface AggregatedResults<T> {
  results: T[];
  errors: Error[];
  successCount: number;
  errorCount: number;
  totalCount: number;
}

/**
 * Configuration loading result
 */
export interface ConfigLoadResult {
  success: boolean;
  error?: string;
  warnings?: string[];
}

/**
 * Container engine installation result
 */
export interface EngineInstallResult {
  installed: boolean;
  version?: string;
  error?: string;
}

/**
 * Error handler context for centralized error handling
 */
export interface ErrorHandlerContext {
  operation: string;
  host?: string;
  service?: string;
  metadata?: Record<string, unknown>;
}

/**
 * Command execution context options
 */
export interface CommandContextOptions {
  verbose?: boolean;
  environment?: string;
  configFile?: string;
  version?: string;
  hosts?: string;
  services?: string;
}

/**
 * Command execution context
 */
export interface CommandContext {
  options: CommandContextOptions;
  args: string[];
  metadata?: Record<string, unknown>;
}

/**
 * Generic command handler function type
 */
export type CommandHandler<T = void> = (
  context: CommandContext,
) => Promise<T> | T;

/**
 * Version management options
 */
export interface VersionOptions {
  version?: string;
  force?: boolean;
}

/**
 * Git file status information
 */
export interface GitFileStatus {
  path: string;
  status: string;
  staged: boolean;
}

/**
 * Parsed mount specification
 */
export interface ParsedMount {
  source: string;
  target: string;
  type?: "volume" | "bind" | "tmpfs";
  options?: string[];
  readonly?: boolean;
}

/**
 * Service filtering options
 */
export interface ServiceFilterOptions {
  services?: string;
  hosts?: string;
  includeAll?: boolean;
}

/**
 * Service grouping options for deployment
 */
export interface ServiceGroupingOptions {
  groupBy?: "service" | "host";
  parallel?: boolean;
  maxConcurrency?: number;
}

/**
 * Kamal proxy deployment options
 */
export interface KamalProxyDeployOptions {
  version?: string;
  port?: number;
  network?: string;
  logLevel?: string;
}
