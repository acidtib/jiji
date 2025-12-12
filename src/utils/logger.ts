import { colors } from "@cliffy/ansi/colors";

export interface LoggerOptions {
  prefix?: string;
  showTimestamp?: boolean;
  maxPrefixLength?: number;
  colors?: boolean;
}

export type LogLevel =
  | "info"
  | "warn"
  | "error"
  | "success"
  | "debug"
  | "trace";

export class Logger {
  private prefix: string;
  private showTimestamp: boolean;
  private maxPrefixLength: number;
  private useColors: boolean;

  constructor(options: LoggerOptions = {}) {
    this.prefix = options.prefix || "";
    this.showTimestamp = options.showTimestamp ?? true;
    this.maxPrefixLength = options.maxPrefixLength || 20;
    this.useColors = options.colors ?? true;
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

    // Truncate or pad the prefix to maintain consistent spacing
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
      default:
        return text;
    }
  }

  private formatMessage(
    level: LogLevel,
    message: string,
    prefix?: string,
  ): string {
    const parts: string[] = [];

    // Add timestamp if enabled
    if (this.showTimestamp) {
      const timestamp = this.formatTimestamp();
      parts.push(this.useColors ? colors.dim(timestamp) : timestamp);
    }

    // Add formatted prefix
    const formattedPrefix = this.formatPrefix(prefix);
    if (formattedPrefix) {
      const coloredPrefix = this.useColors
        ? colors.bold(this.colorize(formattedPrefix, level))
        : formattedPrefix;
      parts.push(coloredPrefix);
    }

    // Add level indicator
    const levelIndicator = `[${level.toUpperCase().padEnd(5)}]`;
    const coloredLevel = this.colorize(levelIndicator, level);
    parts.push(coloredLevel);

    // Add the actual message
    parts.push(message);

    return parts.join(" ");
  }

  private log(level: LogLevel, message: string, prefix?: string): void {
    const formattedMessage = this.formatMessage(level, message, prefix);

    switch (level) {
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

  info(message: string, prefix?: string): void {
    this.log("info", message, prefix);
  }

  success(message: string, prefix?: string): void {
    this.log("success", message, prefix);
  }

  warn(message: string, prefix?: string): void {
    this.log("warn", message, prefix);
  }

  error(message: string, prefix?: string): void {
    this.log("error", message, prefix);
  }

  debug(message: string, prefix?: string): void {
    this.log("debug", message, prefix);
  }

  trace(message: string, prefix?: string): void {
    this.log("trace", message, prefix);
  }

  // command execution logging
  executing(command: string, server?: string): void {
    const prefix = server || "local";
    const message = this.useColors
      ? colors.dim(`$ ${command}`)
      : `$ ${command}`;
    this.log("info", message, prefix);
  }

  // status updates
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
    });
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
    const bar = "█".repeat(filled) + "░".repeat(empty);

    return this.useColors ? colors.cyan(`[${bar}]`) : `[${bar}]`;
  }

  // Group related log messages
  group(title: string, fn: () => void | Promise<void>): void | Promise<void> {
    const separator = "─".repeat(60);
    const groupTitle = this.useColors ? colors.bold(colors.blue(title)) : title;

    console.log("");
    console.log(this.useColors ? colors.dim(separator) : separator);
    console.log(groupTitle);
    console.log(this.useColors ? colors.dim(separator) : separator);

    const result = fn();

    if (result instanceof Promise) {
      return result.then(() => {
        console.log(this.useColors ? colors.dim(separator) : separator);
        console.log("");
      });
    } else {
      console.log(this.useColors ? colors.dim(separator) : separator);
      console.log("");
    }
  }
}

// Default logger instance
export const logger = new Logger();

// Utility functions for quick logging
export const log = {
  info: (message: string, prefix?: string) => logger.info(message, prefix),
  success: (message: string, prefix?: string) =>
    logger.success(message, prefix),
  warn: (message: string, prefix?: string) => logger.warn(message, prefix),
  error: (message: string, prefix?: string) => logger.error(message, prefix),
  debug: (message: string, prefix?: string) => logger.debug(message, prefix),
  trace: (message: string, prefix?: string) => logger.trace(message, prefix),
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
};
