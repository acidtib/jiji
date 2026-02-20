/**
 * HTTP client for the Corrosion API.
 *
 * Uses fetch() for persistent connections. Replaces all curl calls
 * from the bash script.
 */

import type { TransactionResult } from "./types.ts";
import * as log from "./logger.ts";

export class CorrosionClient {
  private baseUrl: string;

  constructor(baseUrl: string) {
    this.baseUrl = baseUrl;
  }

  /**
   * Execute a SQL statement via the Corrosion HTTP API.
   * Using the HTTP API ensures subscription events are triggered
   * for real-time DNS updates.
   */
  async exec(sql: string): Promise<TransactionResult> {
    const response = await fetch(`${this.baseUrl}/v1/transactions`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify([sql]),
    });

    if (!response.ok) {
      const body = await response.text();
      throw new Error(
        `Corrosion exec failed (${response.status}): ${body}`,
      );
    }

    return await response.json() as TransactionResult;
  }

  /**
   * Check if Corrosion API is healthy.
   */
  async health(): Promise<boolean> {
    try {
      const response = await fetch(`${this.baseUrl}/health`);
      return response.ok;
    } catch {
      return false;
    }
  }

  /**
   * Execute SQL and return rows_affected from the first result.
   */
  async execGetRowsAffected(sql: string): Promise<number> {
    try {
      const result = await this.exec(sql);
      return result.results?.[0]?.rows_affected ?? 0;
    } catch (err) {
      log.error("Corrosion exec failed", { error: String(err) });
      return 0;
    }
  }
}
