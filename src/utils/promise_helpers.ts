import { log } from "./logger.ts";

/**
 * Enhanced promise utilities for better error handling in multi-host operations
 *
 * These utilities provide patterns that wait for all operations to complete
 * and collect comprehensive error information instead of failing fast.
 */

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
 * Execute operations with comprehensive error collection
 *
 * Unlike Promise.all() which fails fast, this waits for all operations
 * to complete and returns both successful results and errors.
 *
 * @param operations - Array of async operations to execute
 * @returns Object with successful results and collected errors
 */
export async function executeWithErrorCollection<T>(
  operations: (() => Promise<T>)[],
): Promise<AggregatedResults<T>> {
  type SuccessOutcome = { success: true; result: T; index: number };
  type ErrorOutcome = { success: false; error: Error; index: number };
  type Outcome = SuccessOutcome | ErrorOutcome;

  const promises = operations.map(async (op, index): Promise<Outcome> => {
    try {
      const result = await op();
      return { success: true, result, index };
    } catch (error) {
      return {
        success: false,
        error: error instanceof Error ? error : new Error(String(error)),
        index,
      };
    }
  });

  const outcomes = await Promise.all(promises);

  const results = outcomes
    .filter((o): o is SuccessOutcome => o.success)
    .map((o) => o.result);

  const errors = outcomes
    .filter((o): o is ErrorOutcome => !o.success)
    .map((o) => o.error);

  return {
    results,
    errors,
    successCount: results.length,
    errorCount: errors.length,
    totalCount: operations.length,
  };
}

/**
 * Execute host-based operations with error collection
 *
 * Similar to executeWithErrorCollection but specifically designed for
 * operations that target specific hosts, providing better error reporting.
 *
 * @param hostOperations - Array of operations with associated host names
 * @returns Object with successful results and host-specific errors
 */
export async function executeHostOperations<T>(
  hostOperations: Array<{ host: string; operation: () => Promise<T> }>,
): Promise<
  AggregatedResults<T> & { hostErrors: Array<{ host: string; error: Error }> }
> {
  type SuccessOutcome = { success: true; result: T; host: string };
  type ErrorOutcome = { success: false; error: Error; host: string };
  type Outcome = SuccessOutcome | ErrorOutcome;

  const promises = hostOperations.map(
    async ({ host, operation }): Promise<Outcome> => {
      try {
        const result = await operation();
        return { success: true, result, host };
      } catch (error) {
        return {
          success: false,
          error: error instanceof Error ? error : new Error(String(error)),
          host,
        };
      }
    },
  );

  const outcomes = await Promise.all(promises);

  const results = outcomes
    .filter((o): o is SuccessOutcome => o.success)
    .map((o) => o.result);

  const errors = outcomes
    .filter((o): o is ErrorOutcome => !o.success)
    .map((o) => o.error);

  const hostErrors = outcomes
    .filter((o): o is ErrorOutcome => !o.success)
    .map((o) => ({ host: o.host, error: o.error }));

  return {
    results,
    errors,
    hostErrors,
    successCount: results.length,
    errorCount: errors.length,
    totalCount: hostOperations.length,
  };
}

/**
 * Retry operations with error collection
 *
 * Attempts each operation up to maxRetries times, collecting
 * only final failures after all retries are exhausted.
 *
 * @param operations - Array of async operations to execute
 * @param maxRetries - Maximum number of retry attempts per operation
 * @param retryDelay - Base delay between retries in milliseconds
 * @returns Object with successful results and final errors
 */
export async function executeWithRetryAndErrorCollection<T>(
  operations: (() => Promise<T>)[],
  maxRetries: number = 3,
  retryDelay: number = 1000,
): Promise<AggregatedResults<T>> {
  const retriedOperations = operations.map((op) => async () => {
    let lastError: Error | undefined;

    for (let attempt = 1; attempt <= maxRetries; attempt++) {
      try {
        return await op();
      } catch (error) {
        lastError = error instanceof Error ? error : new Error(String(error));

        if (attempt < maxRetries) {
          await sleep(retryDelay * attempt); // Exponential backoff
        }
      }
    }

    throw lastError;
  });

  return await executeWithErrorCollection(retriedOperations);
}

/**
 * Combine multiple aggregated results
 *
 * Useful for combining results from multiple batches or phases.
 *
 * @param resultSets - Array of aggregated results to combine
 * @returns Combined aggregated results
 */
export function combineAggregatedResults<T>(
  resultSets: AggregatedResults<T>[],
): AggregatedResults<T> {
  const allResults: T[] = [];
  const allErrors: Error[] = [];
  let totalCount = 0;

  for (const resultSet of resultSets) {
    allResults.push(...resultSet.results);
    allErrors.push(...resultSet.errors);
    totalCount += resultSet.totalCount;
  }

  return {
    results: allResults,
    errors: allErrors,
    successCount: allResults.length,
    errorCount: allErrors.length,
    totalCount,
  };
}

/**
 * Create error summary for reporting
 *
 * Generates a human-readable summary of aggregated results.
 *
 * @param results - Aggregated results to summarize
 * @param operation - Name of the operation for context
 * @returns Formatted summary string
 */
export function createErrorSummary<T>(
  results: AggregatedResults<T>,
  operation: string,
): string {
  const { successCount, errorCount, totalCount } = results;

  if (errorCount === 0) {
    return `${operation} completed successfully on all ${totalCount} target(s)`;
  }

  if (successCount === 0) {
    return `${operation} failed on all ${totalCount} target(s)`;
  }

  return `${operation} completed with mixed results: ${successCount} succeeded, ${errorCount} failed (total: ${totalCount})`;
}

/**
 * Log aggregated results with appropriate log levels
 *
 * @param results - Aggregated results to log
 * @param operation - Name of the operation for context
 * @param logger - Logger function (defaults to console)
 */
export function logAggregatedResults<T>(
  results: AggregatedResults<T>,
  operation: string,
  logger: {
    success: (msg: string) => void;
    warn: (msg: string) => void;
    error: (msg: string) => void;
  } = {
    success: (msg) => log.success(`${msg}`, "promise-helpers"),
    warn: (msg) => log.warn(`${msg}`, "promise-helpers"),
    error: (msg) => log.error(`${msg}`, "promise-helpers"),
  },
): void {
  const summary = createErrorSummary(results, operation);

  if (results.errorCount === 0) {
    logger.success(summary);
  } else if (results.successCount === 0) {
    logger.error(summary);
  } else {
    logger.warn(summary);
  }

  // Log individual errors
  if (results.errors.length > 0) {
    logger.error(`Errors encountered during ${operation}:`);
    results.errors.forEach((error, index) => {
      logger.error(`  ${index + 1}. ${error.message}`);
    });
  }
}

/**
 * Sleep utility for retry delays
 */
function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
