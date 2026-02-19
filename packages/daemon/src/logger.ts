/**
 * Structured JSON logger for the control loop.
 *
 * All output is JSON for machine-parseable logs via journald.
 */

let _serverId = "unknown";

export function setServerId(id: string): void {
  _serverId = id;
}

interface LogEntry {
  timestamp: string;
  level: string;
  server_id: string;
  message: string;
  data?: Record<string, unknown>;
}

function emit(
  level: string,
  message: string,
  data?: Record<string, unknown>,
): void {
  const entry: LogEntry = {
    timestamp: new Date().toISOString(),
    level,
    server_id: _serverId,
    message,
  };
  if (data !== undefined) {
    entry.data = data;
  }
  console.log(JSON.stringify(entry));
}

export function info(message: string, data?: Record<string, unknown>): void {
  emit("info", message, data);
}

export function warn(message: string, data?: Record<string, unknown>): void {
  emit("warn", message, data);
}

export function error(message: string, data?: Record<string, unknown>): void {
  emit("error", message, data);
}

export function debug(message: string, data?: Record<string, unknown>): void {
  emit("debug", message, data);
}
