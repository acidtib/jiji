/**
 * Semaphore for limiting concurrent operations
 */
class Semaphore {
  private permits: number;
  private queue: (() => void)[] = [];

  constructor(permits: number) {
    this.permits = permits;
  }

  acquire(): Promise<void> {
    if (this.permits > 0) {
      this.permits--;
      return Promise.resolve();
    }

    return new Promise<void>((resolve) => {
      this.queue.push(resolve);
    });
  }

  release(): void {
    this.permits++;
    const next = this.queue.shift();
    if (next) {
      this.permits--;
      next();
    }
  }

  /**
   * Get current number of available permits
   */
  available(): number {
    return this.permits;
  }

  /**
   * Get current queue length
   */
  queueLength(): number {
    return this.queue.length;
  }
}

/**
 * SSH Connection Pool with concurrency control
 *
 * Limits the number of concurrent SSH operations to prevent
 * overwhelming servers during large deployments.
 */
export class SSHConnectionPool {
  private semaphore: Semaphore;
  private maxConcurrent: number;

  constructor(maxConcurrent: number = 30) {
    this.maxConcurrent = maxConcurrent;
    this.semaphore = new Semaphore(maxConcurrent);
  }

  /**
   * Execute operations with concurrency limit
   *
   * All operations are started immediately but the semaphore
   * ensures only `maxConcurrent` are running at the same time.
   *
   * @param operations - Array of async operations to execute
   * @returns Promise that resolves with all results
   */
  executeConcurrent<T>(
    operations: (() => Promise<T>)[],
  ): Promise<T[]> {
    return Promise.all(
      operations.map(async (op) => {
        await this.semaphore.acquire();
        try {
          return await op();
        } finally {
          this.semaphore.release();
        }
      }),
    );
  }

  /**
   * Execute operations in batches
   *
   * Processes operations in sequential batches of size `batchSize`.
   * Each batch waits for all operations to complete before starting the next.
   *
   * @param operations - Array of async operations to execute
   * @param batchSize - Size of each batch (defaults to maxConcurrent)
   * @returns Promise that resolves with all results
   */
  async executeBatched<T>(
    operations: (() => Promise<T>)[],
    batchSize?: number,
  ): Promise<T[]> {
    const size = batchSize || this.maxConcurrent;
    const results: T[] = [];

    for (let i = 0; i < operations.length; i += size) {
      const batch = operations.slice(i, i + size);
      const batchResults = await Promise.all(batch.map((op) => op()));
      results.push(...batchResults);
    }

    return results;
  }

  /**
   * Execute operations with error collection
   *
   * Unlike executeConcurrent which fails fast on first error,
   * this method collects all errors and successful results.
   *
   * @param operations - Array of async operations to execute
   * @returns Object with successful results and errors
   */
  async executeWithErrorCollection<T>(
    operations: (() => Promise<T>)[],
  ): Promise<{ results: T[]; errors: Error[] }> {
    type Outcome =
      | { success: true; result: T }
      | { success: false; error: Error };

    const outcomes: Outcome[] = [];

    await this.executeConcurrent(
      operations.map((op) => async () => {
        try {
          const result = await op();
          outcomes.push({ success: true, result });
        } catch (error) {
          outcomes.push({
            success: false,
            error: error instanceof Error ? error : new Error(String(error)),
          });
        }
      }),
    );

    const results = outcomes
      .filter((o): o is { success: true; result: T } => o.success)
      .map((o) => o.result);

    const errors = outcomes
      .filter((o): o is { success: false; error: Error } => !o.success)
      .map((o) => o.error);

    return { results, errors };
  }

  /**
   * Get pool statistics
   */
  getStats(): {
    maxConcurrent: number;
    available: number;
    queued: number;
  } {
    return {
      maxConcurrent: this.maxConcurrent,
      available: this.semaphore.available(),
      queued: this.semaphore.queueLength(),
    };
  }
}
