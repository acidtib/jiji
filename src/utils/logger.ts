import { colors } from "@cliffy/ansi/colors";
import type { LoggerOptions, LogLevel } from "../types.ts";

const LOG_LEVEL_PRIORITY: Record<LogLevel, number> = {
  fatal: 0,
  error: 1,
  warn: 2,
  info: 3,
  success: 3, // Same as info
  debug: 4,
  trace: 5,
};

let globalMinLevel: LogLevel | undefined = undefined;
let globalQuietMode = false;

/**
 * Set the global minimum log level for all loggers
 */
export function setGlobalLogLevel(level: LogLevel): void {
  globalMinLevel = level;
}

/**
 * Get the current global log level
 */
export function getGlobalLogLevel(): LogLevel | undefined {
  return globalMinLevel;
}

/**
 * Set global quiet mode for all loggers
 */
export function setGlobalQuietMode(enabled: boolean): void {
  globalQuietMode = enabled;
}

/**
 * Get global quiet mode status
 */
export function getGlobalQuietMode(): boolean {
  return globalQuietMode;
}

export class Logger {
  private prefix: string;
  private showTimestamp: boolean;
  private maxPrefixLength: number;
  private useColors: boolean;
  private minLevel: LogLevel;
  private quietMode: boolean;

  constructor(options: LoggerOptions = {}) {
    this.prefix = options.prefix || "";
    this.showTimestamp = options.showTimestamp ?? false; // Changed default to false
    this.maxPrefixLength = options.maxPrefixLength || 20;
    this.useColors = options.colors ?? true;
    this.minLevel = options.minLevel || globalMinLevel || "info";
    this.quietMode = options.quiet ?? globalQuietMode;
  }

  setMinLevel(level: LogLevel): void {
    this.minLevel = level;
  }

  private formatTimestamp(): string {
    const now = new Date();
    const hours = now.getHours().toString().padStart(2, "0");
    const minutes = now.getMinutes().toString().padStart(2, "0");
    const seconds = now.getSeconds().toString().padStart(2, "0");
    const ms = now.getMilliseconds().toString().padStart(3, "0");
    return `${hours}:${minutes}:${seconds}.${ms}`;
  }

  private formatPrefix(prefix?: string): string {
    const actualPrefix = prefix || this.prefix;
    if (!actualPrefix) return "";

    const truncated = actualPrefix.length > this.maxPrefixLength
      ? actualPrefix.substring(0, this.maxPrefixLength - 3) + "..."
      : actualPrefix;

    return truncated.padEnd(this.maxPrefixLength);
  }

  private colorize(text: string, level: LogLevel): string {
    if (!this.useColors) return text;

    switch (level) {
      case "info":
        return colors.cyan(text);
      case "success":
        return colors.green(text);
      case "warn":
        return colors.yellow(text);
      case "error":
        return colors.red(text);
      case "debug":
        return colors.magenta(text);
      case "trace":
        return colors.gray(text);
      case "fatal":
        return colors.brightRed(text);
      default:
        return text;
    }
  }

  private formatMessage(
    level: LogLevel,
    message: string,
    prefix?: string,
    indent?: number,
  ): string {
    const parts: string[] = [];

    // Add indentation if specified
    const indentation = indent !== undefined ? "  ".repeat(indent) : "";

    if (this.showTimestamp) {
      const timestamp = this.formatTimestamp();
      parts.push(this.useColors ? colors.dim(timestamp) : timestamp);
    }

    const formattedPrefix = this.formatPrefix(prefix);
    if (formattedPrefix) {
      const coloredPrefix = this.useColors
        ? colors.bold(this.colorize(formattedPrefix, level))
        : formattedPrefix;
      parts.push(coloredPrefix);
    }

    // Only show level indicator for warn, error, and fatal
    if (level === "warn" || level === "error" || level === "fatal") {
      const levelIndicator = `[${level.toUpperCase().padEnd(5)}]`;
      const coloredLevel = this.colorize(levelIndicator, level);
      parts.push(coloredLevel);
    }

    parts.push(message);

    return indentation + parts.join(" ");
  }

  private log(
    level: LogLevel,
    message: string,
    prefix?: string,
    indent?: number,
  ): void {
    if (!this.shouldLog(level)) {
      return;
    }

    const formattedMessage = this.formatMessage(level, message, prefix, indent);

    switch (level) {
      case "fatal":
      case "error":
        console.error(formattedMessage);
        break;
      case "warn":
        console.warn(formattedMessage);
        break;
      default:
        console.log(formattedMessage);
        break;
    }
  }

  /**
   * Check if a log level should be output based on the minimum level
   */
  private shouldLog(level: LogLevel): boolean {
    return LOG_LEVEL_PRIORITY[level] <= LOG_LEVEL_PRIORITY[this.minLevel];
  }

  info(message: string, prefixOrIndent?: string | number): void {
    const { prefix, indent } = this.parseLogParams(prefixOrIndent);
    this.log("info", message, prefix, indent);
  }

  success(message: string, prefixOrIndent?: string | number): void {
    const { prefix, indent } = this.parseLogParams(prefixOrIndent);
    this.log("success", message, prefix, indent);
  }

  warn(message: string, prefixOrIndent?: string | number): void {
    const { prefix, indent } = this.parseLogParams(prefixOrIndent);
    this.log("warn", message, prefix, indent);
  }

  error(message: string, prefixOrIndent?: string | number): void {
    const { prefix, indent } = this.parseLogParams(prefixOrIndent);
    this.log("error", message, prefix, indent);
  }

  debug(message: string, prefixOrIndent?: string | number): void {
    const { prefix, indent } = this.parseLogParams(prefixOrIndent);
    this.log("debug", message, prefix, indent);
  }

  trace(message: string, prefixOrIndent?: string | number): void {
    const { prefix, indent } = this.parseLogParams(prefixOrIndent);
    this.log("trace", message, prefix, indent);
  }

  fatal(message: string, prefixOrIndent?: string | number): void {
    const { prefix, indent } = this.parseLogParams(prefixOrIndent);
    this.log("fatal", message, prefix, indent);
  }

  private parseLogParams(
    prefixOrIndent?: string | number,
  ): { prefix?: string; indent?: number } {
    if (typeof prefixOrIndent === "number") {
      return { indent: prefixOrIndent };
    }
    // Ignore string parameters - we're moving away from prefix
    return {};
  }

  executing(command: string, server?: string): void {
    const prefix = server || "local";
    const message = this.useColors
      ? colors.dim(`$ ${command}`)
      : `$ ${command}`;
    this.log("info", message, prefix);
  }

  status(message: string, server?: string): void {
    const prefix = server || "status";
    this.log("info", message, prefix);
  }

  // Create a child logger with a specific prefix
  child(prefix: string): Logger {
    return new Logger({
      prefix,
      showTimestamp: this.showTimestamp,
      maxPrefixLength: this.maxPrefixLength,
      colors: this.useColors,
      minLevel: this.minLevel,
      quiet: this.quietMode,
    });
  }

  /**
   * Create a logger with a specific minimum level
   */
  withLevel(level: LogLevel): Logger {
    return new Logger({
      prefix: this.prefix,
      showTimestamp: this.showTimestamp,
      maxPrefixLength: this.maxPrefixLength,
      colors: this.useColors,
      minLevel: level,
      quiet: this.quietMode,
    });
  }

  /**
   * Display output grouped by host with a clear header
   * Inspired by kamal's puts_by_host pattern
   */
  hostOutput(host: string, output: string, options: {
    type?: string;
    skipHeader?: boolean;
  } = {}): void {
    if (this.quietMode && !options.skipHeader) {
      // In quiet mode, skip the header but show the output
      if (output.trim()) {
        console.log(output);
      }
      return;
    }

    const type = options.type || "Host";

    if (!options.skipHeader) {
      const header = `${type}: ${host}`;
      const coloredHeader = this.useColors
        ? colors.bold(colors.cyan(header))
        : header;
      console.log(coloredHeader);
    }

    if (output.trim()) {
      console.log(output);
    }
    console.log(""); // Empty line after output
  }

  /**
   * Display a simple action message (like kamal's "say")
   */
  action(
    message: string,
    color: "cyan" | "magenta" | "yellow" | "green" | "red" = "cyan",
  ): void {
    if (this.quietMode) return;

    let coloredMessage = message;
    if (this.useColors) {
      switch (color) {
        case "cyan":
          coloredMessage = colors.cyan(message);
          break;
        case "magenta":
          coloredMessage = colors.magenta(message);
          break;
        case "yellow":
          coloredMessage = colors.yellow(message);
          break;
        case "green":
          coloredMessage = colors.green(message);
          break;
        case "red":
          coloredMessage = colors.red(message);
          break;
      }
    }
    console.log(coloredMessage);
  }

  /**
   * Display a command being executed (dimmed)
   */
  command(cmd: string, host?: string): void {
    if (!this.shouldLog("debug")) return;

    const prefix = host ? `${host}` : "local";
    const message = this.useColors ? colors.dim(`$ ${cmd}`) : `$ ${cmd}`;
    const parts: string[] = [];

    if (prefix) {
      parts.push(this.useColors ? colors.bold(colors.cyan(prefix)) : prefix);
    }
    parts.push(message);

    console.log(parts.join(" "));
  }

  /**
   * Display raw output without any formatting
   */
  raw(output: string): void {
    console.log(output);
  }

  // Create multiple loggers for different servers
  static forServers(
    servers: string[],
    options: LoggerOptions = {},
  ): Map<string, Logger> {
    const loggers = new Map<string, Logger>();

    for (const server of servers) {
      loggers.set(
        server,
        new Logger({
          ...options,
          prefix: server,
        }),
      );
    }

    return loggers;
  }

  /**
   * Create a logger configured for SSH operations with the specified log level
   */
  static forSSH(
    logLevel: LogLevel = "error",
    options: LoggerOptions = {},
  ): Logger {
    return new Logger({
      ...options,
      minLevel: logLevel,
      prefix: options.prefix || "ssh",
    });
  }

  // Progress indicator for long-running tasks
  progress(
    message: string,
    current: number,
    total: number,
    prefix?: string,
  ): void {
    const percentage = Math.round((current / total) * 100);
    const progressBar = this.createProgressBar(current, total);
    const progressMessage =
      `${message} ${progressBar} ${percentage}% (${current}/${total})`;
    this.log("info", progressMessage, prefix);
  }

  private createProgressBar(
    current: number,
    total: number,
    width = 20,
  ): string {
    const filled = Math.round((current / total) * width);
    const empty = width - filled;
    const bar = "#".repeat(filled) + ".".repeat(empty);

    return this.useColors ? colors.cyan(`[${bar}]`) : `[${bar}]`;
  }

  // Group related log messages
  group(title: string, fn: () => void | Promise<void>): void | Promise<void> {
    if (this.quietMode) {
      return fn();
    }

    console.log("");
    this.section(title);

    const result = fn();

    if (result instanceof Promise) {
      return result.then(() => {
        // Promise resolved, grouping complete
      });
    }
    // Synchronous function completed, grouping complete
  }

  /**
   * Execute a block of operations for a specific host
   * Prints host header and indents content
   */
  async hostBlock(
    host: string,
    fn: () => Promise<void> | void,
    options: { indent?: number } = {},
  ): Promise<void> {
    if (this.quietMode) {
      await fn();
      return;
    }

    const indent = options.indent || 0;
    const indentation = "  ".repeat(indent);

    // Print host header
    console.log(
      `${indentation}${this.useColors ? colors.bold(colors.cyan(host)) : host}`,
    );

    await fn();
  }

  /**
   * Display a step in a multi-step process
   * Similar to kamal's step logging
   */
  step(message: string, indent = 0): void {
    if (this.quietMode) return;

    const indentation = "  ".repeat(indent);
    const bullet = this.useColors ? colors.cyan("-") : "-";
    console.log(`${indentation}${bullet} ${message}`);
  }

  /**
   * Display remote command execution in kamal style
   * Shows: Running command on server (dimmed command)
   */
  remote(
    host: string,
    message: string,
    options: { indent?: number; command?: string } = {},
  ): void {
    const indent = "  ".repeat(options.indent || 0);
    const hostLabel = this.useColors ? colors.bold(colors.cyan(host)) : host;

    if (options.command && this.shouldLog("debug")) {
      const cmd = this.useColors
        ? colors.dim(`$ ${options.command}`)
        : `$ ${options.command}`;
      console.log(`${indent}${hostLabel} ${message}`);
      console.log(`${indent}  ${cmd}`);
    } else {
      console.log(`${indent}${hostLabel} ${message}`);
    }
  }

  /**
   * Start a new section with clear visual separation
   * Kamal-style section headers
   */
  section(title: string): void {
    if (this.quietMode) return;

    console.log(""); // Blank line before
    const formatted = this.useColors ? colors.bold(title) : title;
    console.log(formatted);
  }

  /**
   * Print a summary line (like kamal's "say")
   */
  say(message: string, indent = 0): void {
    if (this.quietMode) return;

    const indentation = "  ".repeat(indent);
    console.log(`${indentation}${message}`);
  }

  /**
   * Create a step tracker for multi-step operations
   * Returns functions to log steps and finish
   */
  createStepTracker(title: string): {
    step: (message: string, indent?: number) => void;
    remote: (
      host: string,
      message: string,
      options?: { indent?: number; command?: string },
    ) => void;
    finish: (success?: boolean) => void;
  } {
    let hasOutput = false;

    if (!this.quietMode) {
      this.section(title);
      hasOutput = true;
    }

    return {
      step: (message: string, indent = 0) => {
        if (!hasOutput && !this.quietMode) {
          this.section(title);
          hasOutput = true;
        }
        this.step(message, indent);
      },
      remote: (host: string, message: string, options = {}) => {
        if (!hasOutput && !this.quietMode) {
          this.section(title);
          hasOutput = true;
        }
        this.remote(host, message, options);
      },
      finish: (_success = true) => {
        // No-op: timing removed
      },
    };
  }
}

/**
 * Tree prefix utilities for hierarchical output
 */

/**
 * Get the appropriate tree prefix character based on position
 * @param isLast Whether this is the last item in a list
 * @returns Tree prefix string ("├──" or "└──")
 */
export function treePrefix(isLast: boolean): string {
  return isLast ? "└──" : "├──";
}

/**
 * TreeLogger class for managing hierarchical tree-like output
 * Automatically handles prefix selection based on item position
 */
export class TreeLogger {
  private items: string[] = [];

  /**
   * Add an item to the tree
   */
  add(message: string): void {
    this.items.push(message);
  }

  /**
   * Print all items with appropriate tree prefixes and clear the list
   * @param indent Indentation level (default: 2)
   */
  print(indent: number = 2): void {
    this.items.forEach((item, index) => {
      const isLast = index === this.items.length - 1;
      const prefix = treePrefix(isLast);
      log.say(`${prefix} ${item}`, indent);
    });
    this.items = [];
  }

  /**
   * Get the number of items currently in the tree
   */
  get length(): number {
    return this.items.length;
  }

  /**
   * Clear all items without printing
   */
  clear(): void {
    this.items = [];
  }
}

// Default logger instance
export const logger = new Logger();

// Utility functions for quick logging
export const log = {
  info: (message: string, prefixOrIndent?: string | number) =>
    logger.info(message, prefixOrIndent),
  success: (message: string, prefixOrIndent?: string | number) =>
    logger.success(message, prefixOrIndent),
  warn: (message: string, prefixOrIndent?: string | number) =>
    logger.warn(message, prefixOrIndent),
  error: (message: string, prefixOrIndent?: string | number) =>
    logger.error(message, prefixOrIndent),
  fatal: (message: string, prefixOrIndent?: string | number) =>
    logger.fatal(message, prefixOrIndent),
  debug: (message: string, prefixOrIndent?: string | number) =>
    logger.debug(message, prefixOrIndent),
  trace: (message: string, prefixOrIndent?: string | number) =>
    logger.trace(message, prefixOrIndent),
  executing: (command: string, server?: string) =>
    logger.executing(command, server),
  status: (message: string, server?: string) => logger.status(message, server),
  progress: (
    message: string,
    current: number,
    total: number,
    prefix?: string,
  ) => logger.progress(message, current, total, prefix),
  group: (title: string, fn: () => void | Promise<void>) =>
    logger.group(title, fn),
  hostOutput: (host: string, output: string, options?: {
    type?: string;
    skipHeader?: boolean;
  }) => logger.hostOutput(host, output, options),
  action: (
    message: string,
    color?: "cyan" | "magenta" | "yellow" | "green" | "red",
  ) => logger.action(message, color),
  command: (cmd: string, host?: string) => logger.command(cmd, host),
  raw: (output: string) => logger.raw(output),
  hostBlock: (
    host: string,
    fn: () => Promise<void> | void,
    options?: { indent?: number },
  ) => logger.hostBlock(host, fn, options),
  step: (message: string, indent?: number) => logger.step(message, indent),
  remote: (
    host: string,
    message: string,
    options?: { indent?: number; command?: string },
  ) => logger.remote(host, message, options),
  section: (title: string) => logger.section(title),
  say: (message: string, indent?: number) => logger.say(message, indent),
  createStepTracker: (title: string) => logger.createStepTracker(title),
  setMinLevel: (level: LogLevel) => logger.setMinLevel(level),
};
