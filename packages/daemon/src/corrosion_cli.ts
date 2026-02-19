/**
 * CLI query wrapper for the Corrosion binary.
 *
 * Uses `corrosion query` for read operations that don't need
 * to trigger subscription events.
 */

import * as log from "./logger.ts";

export class CorrosionCli {
  private corrosionDir: string;

  constructor(corrosionDir: string) {
    this.corrosionDir = corrosionDir;
  }

  /**
   * Execute a SQL query via the corrosion CLI binary.
   * Returns rows as string arrays (pipe-delimited output).
   */
  async query(sql: string): Promise<string[][]> {
    const cmd = new Deno.Command(`${this.corrosionDir}/corrosion`, {
      args: ["query", "--config", `${this.corrosionDir}/config.toml`, sql],
      stdout: "piped",
      stderr: "piped",
    });

    const output = await cmd.output();

    if (!output.success) {
      const stderr = new TextDecoder().decode(output.stderr);
      log.error("Corrosion CLI query failed", { sql, stderr });
      return [];
    }

    const stdout = new TextDecoder().decode(output.stdout).trim();
    if (stdout === "") {
      return [];
    }

    return stdout.split("\n").map((line) => line.split("|"));
  }

  /**
   * Execute a scalar query (returns single value from first row).
   */
  async queryScalar(sql: string): Promise<string | null> {
    const rows = await this.query(sql);
    if (rows.length === 0 || rows[0].length === 0) {
      return null;
    }
    return rows[0][0];
  }
}
