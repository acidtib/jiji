/**
 * Corrosion subscription client
 *
 * Connects to Corrosion's /v1/subscriptions endpoint and streams
 * real-time updates about container changes.
 */

import type {
  CorrosionChangeMessage,
  CorrosionMessage,
  CorrosionRowMessage,
  DnsRecord,
  SubscriberEvents,
} from "./types.ts";

/**
 * SQL query for subscribing to container changes
 *
 * Selects containers joined with services to get project info.
 * Only returns healthy containers.
 */
const SUBSCRIPTION_QUERY = `
SELECT
  c.id,
  c.service,
  c.server_id,
  c.ip,
  c.health_status,
  c.started_at,
  c.instance_id,
  s.project
FROM containers c
JOIN services s ON c.service = s.name
WHERE c.health_status = 'healthy'
`.trim();

/**
 * Column indices in the subscription query result
 */
const COLUMNS = {
  ID: 0,
  SERVICE: 1,
  SERVER_ID: 2,
  IP: 3,
  HEALTH_STATUS: 4,
  STARTED_AT: 5,
  INSTANCE_ID: 6,
  PROJECT: 7,
};

/**
 * Corrosion subscription client
 *
 * Maintains a streaming HTTP connection to Corrosion and emits
 * events when containers are added, updated, or removed.
 */
export class CorrosionSubscriber {
  private corrosionApi: string;
  private events: SubscriberEvents;
  private reconnectInterval: number;
  private maxReconnectAttempts: number;
  private abortController: AbortController | null = null;
  private reconnectAttempt = 0;
  private isRunning = false;

  constructor(
    corrosionApi: string,
    events: SubscriberEvents,
    reconnectInterval = 5000,
    maxReconnectAttempts = 0, // 0 = unlimited
  ) {
    this.corrosionApi = corrosionApi;
    this.events = events;
    this.reconnectInterval = reconnectInterval;
    this.maxReconnectAttempts = maxReconnectAttempts;
  }

  /**
   * Start the subscription
   *
   * Connects to Corrosion and begins streaming updates.
   * Automatically reconnects on connection loss.
   */
  async start(): Promise<void> {
    this.isRunning = true;
    await this.connect();
  }

  /**
   * Stop the subscription
   *
   * Closes the connection and stops reconnection attempts.
   */
  stop(): void {
    this.isRunning = false;
    if (this.abortController) {
      this.abortController.abort();
      this.abortController = null;
    }
  }

  /**
   * Connect to Corrosion subscription endpoint
   */
  private async connect(): Promise<void> {
    if (!this.isRunning) {
      return;
    }

    try {
      this.abortController = new AbortController();

      const response = await fetch(`${this.corrosionApi}/v1/subscriptions`, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
        },
        body: JSON.stringify({
          id: "jiji-dns",
          query: SUBSCRIPTION_QUERY,
        }),
        signal: this.abortController.signal,
      });

      if (!response.ok) {
        throw new Error(`Subscription failed: ${response.status} ${response.statusText}`);
      }

      if (!response.body) {
        throw new Error("No response body");
      }

      // Reset reconnect attempts on successful connection
      this.reconnectAttempt = 0;

      // Process the NDJSON stream
      await this.processStream(response.body);
    } catch (error) {
      if (error instanceof Error && error.name === "AbortError") {
        // Connection was intentionally aborted
        return;
      }

      this.events.onError(error instanceof Error ? error : new Error(String(error)));
      await this.scheduleReconnect();
    }
  }

  /** Maximum buffer size (1 MB) to prevent unbounded memory growth */
  private static readonly MAX_BUFFER_SIZE = 1024 * 1024;

  /**
   * Process the NDJSON stream from Corrosion
   */
  private async processStream(body: ReadableStream<Uint8Array>): Promise<void> {
    const reader = body.getReader();
    const decoder = new TextDecoder();
    let buffer = "";

    try {
      while (this.isRunning) {
        const { done, value } = await reader.read();

        if (done) {
          // Stream ended, schedule reconnect
          break;
        }

        buffer += decoder.decode(value, { stream: true });

        // Guard against unbounded buffer growth
        if (buffer.length > CorrosionSubscriber.MAX_BUFFER_SIZE) {
          console.error("Stream buffer exceeded 1 MB, dropping connection");
          buffer = "";
          break;
        }

        // Process complete lines
        let newlineIndex: number;
        while ((newlineIndex = buffer.indexOf("\n")) !== -1) {
          const line = buffer.slice(0, newlineIndex).trim();
          buffer = buffer.slice(newlineIndex + 1);

          if (line.length > 0) {
            this.processLine(line);
          }
        }
      }
    } finally {
      reader.releaseLock();
    }

    // If we get here and still running, schedule reconnect
    if (this.isRunning) {
      await this.scheduleReconnect();
    }
  }

  /**
   * Process a single NDJSON line
   */
  private processLine(line: string): void {
    try {
      const message = JSON.parse(line) as CorrosionMessage;

      if ("columns" in message) {
        // Column names message - we use fixed indices so no action needed
      } else if ("row" in message) {
        this.handleRow(message);
      } else if ("change" in message) {
        this.handleChange(message);
      } else if ("eoq" in message) {
        // End of initial query, all existing data has been sent
        this.events.onReady();
      }
    } catch (error) {
      console.error(`Failed to parse line: ${line}`, error);
    }
  }

  /**
   * Handle single row message (initial data)
   */
  private handleRow(message: CorrosionRowMessage): void {
    // message.row is [rowIndex, [values...]]
    const [_rowIndex, values] = message.row;
    const record = this.rowToRecord(values);
    if (record) {
      this.events.onUpsert(record);
    }
  }

  /**
   * Handle change message (real-time updates)
   *
   * Corrosion sends changes as arrays:
   * - ["insert", rowIndex, [values...], changeId]
   * - ["update", rowIndex, [values...], changeId]
   * - ["delete", rowIndex, [primaryKeys...], changeId]
   */
  private handleChange(message: CorrosionChangeMessage): void {
    const change = message.change;

    // change is an array: [operation, rowIndex, values, changeId]
    const [operation, _rowIndex, values, _changeId] = change;

    if (operation === "insert" || operation === "update") {
      const record = this.rowToRecord(values);
      if (record) {
        this.events.onUpsert(record);
      }
    } else if (operation === "delete") {
      // For delete, values contains the primary key(s)
      const containerId = values[0];
      if (typeof containerId === "string") {
        this.events.onDelete(containerId);
      }
    }
  }

  /**
   * Convert a row array to a DnsRecord
   */
  private rowToRecord(row: (string | number | null)[]): DnsRecord | null {
    const id = row[COLUMNS.ID];
    const service = row[COLUMNS.SERVICE];
    const serverId = row[COLUMNS.SERVER_ID];
    const ip = row[COLUMNS.IP];
    const healthStatus = row[COLUMNS.HEALTH_STATUS];
    const startedAt = row[COLUMNS.STARTED_AT];
    const instanceId = row[COLUMNS.INSTANCE_ID];
    const project = row[COLUMNS.PROJECT];

    // Validate required fields
    if (
      typeof id !== "string" ||
      typeof service !== "string" ||
      typeof serverId !== "string" ||
      typeof ip !== "string" ||
      typeof project !== "string"
    ) {
      return null;
    }

    // Validate IPv4 format (4 octets, each 0-255)
    const octets = ip.split(".");
    if (
      octets.length !== 4 ||
      !octets.every((o) => {
        const n = parseInt(o, 10);
        return !isNaN(n) && n >= 0 && n <= 255 && String(n) === o;
      })
    ) {
      console.warn(`Invalid IP address from Corrosion: ${ip}`);
      return null;
    }

    // Determine health from health_status column
    const isHealthy = healthStatus === "healthy";

    return {
      containerId: id,
      service,
      project,
      serverId,
      ip,
      healthy: isHealthy,
      startedAt: typeof startedAt === "number" ? startedAt : Date.now(),
      instanceId: typeof instanceId === "string" ? instanceId : undefined,
    };
  }

  /**
   * Schedule a reconnection attempt
   */
  private async scheduleReconnect(): Promise<void> {
    if (!this.isRunning) {
      return;
    }

    this.reconnectAttempt++;

    if (this.maxReconnectAttempts > 0 && this.reconnectAttempt > this.maxReconnectAttempts) {
      this.events.onError(
        new Error(`Max reconnect attempts (${this.maxReconnectAttempts}) exceeded`),
      );
      return;
    }

    this.events.onReconnect(this.reconnectAttempt);

    // Exponential backoff with jitter
    const baseDelay = Math.min(
      this.reconnectInterval * Math.pow(2, this.reconnectAttempt - 1),
      60000,
    );
    const jitter = Math.random() * 1000;
    const delay = baseDelay + jitter;

    await new Promise((resolve) => setTimeout(resolve, delay));

    if (this.isRunning) {
      await this.connect();
    }
  }
}
