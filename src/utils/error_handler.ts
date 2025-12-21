/**
 * Standardized error handling utilities for commands
 */

import { log } from "./logger.ts";
import type { SSHManager } from "./ssh.ts";
import { createServerAuditLogger } from "./audit.ts";

/**
 * Context for error handling
 */
export interface ErrorHandlerContext {
  /**
   * The operation being performed (e.g., "Bootstrap", "Deployment")
   */
  operation: string;
  /**
   * The component identifier for logging (e.g., "bootstrap", "deploy")
   */
  component: string;
  /**
   * SSH managers for audit logging (optional)
   */
  sshManagers?: SSHManager[];
  /**
   * Project name for audit logging (optional)
   */
  projectName?: string;
  /**
   * Target hosts for audit logging (optional)
   */
  targetHosts?: string[];
  /**
   * Additional audit payload (optional)
   */
  auditPayload?: Record<string, unknown>;
  /**
   * Custom audit logger function (optional)
   * If provided, this will be called instead of default audit logging
   */
  customAuditLogger?: (errorMessage: string) => Promise<void>;
}

/**
 * Handle command errors with consistent logging and audit trail
 *
 * This function:
 * 1. Logs the error to console
 * 2. Optionally logs to audit trail on servers
 * 3. Exits the process with code 1
 *
 * @param error The error that occurred
 * @param context Error handling context
 * @returns Never returns (exits process)
 *
 * @example
 * ```typescript
 * try {
 *   // ... command logic
 * } catch (error) {
 *   await handleCommandError(error, {
 *     operation: "Bootstrap",
 *     component: "bootstrap",
 *     sshManagers,
 *     projectName: config.project,
 *     targetHosts: ctx.targetHosts,
 *   });
 * }
 * ```
 */
export async function handleCommandError(
  error: unknown,
  context: ErrorHandlerContext,
): Promise<never> {
  const errorMessage = error instanceof Error ? error.message : String(error);

  // Log error to console
  log.error(`${context.operation} failed:`, context.component);
  log.error(errorMessage, context.component);

  // Log to audit trail if SSH managers are available
  if (context.customAuditLogger) {
    // Use custom audit logger
    try {
      await context.customAuditLogger(errorMessage);
    } catch (auditError) {
      log.debug(
        `Failed to log to audit trail: ${auditError}`,
        context.component,
      );
    }
  } else if (
    context.sshManagers &&
    context.projectName &&
    context.targetHosts
  ) {
    // Use default audit logging
    try {
      await logFailureToAudit(
        context.sshManagers,
        context.projectName,
        context.targetHosts,
        errorMessage,
        context.component,
      );
    } catch (auditError) {
      log.debug(
        `Failed to log to audit trail: ${auditError}`,
        context.component,
      );
    }
  }

  Deno.exit(1);
}

/**
 * Log failure to audit trail on all servers
 *
 * @param sshManagers SSH managers for connected hosts
 * @param projectName Project name
 * @param targetHosts Target hosts
 * @param errorMessage Error message
 * @param component Component identifier
 */
async function logFailureToAudit(
  sshManagers: SSHManager[],
  projectName: string,
  targetHosts: string[],
  errorMessage: string,
  component: string,
): Promise<void> {
  // Log the failure with a generic custom command entry
  // This allows flexibility for different operation types
  const results = await Promise.allSettled(
    targetHosts.map(async (host) => {
      const hostSsh = sshManagers.find((ssh) => ssh.getHost() === host);
      if (hostSsh) {
        const hostLogger = createServerAuditLogger(hostSsh, projectName);
        await hostLogger.logCustomCommand(
          component,
          "failed",
          errorMessage,
        );
      }
    }),
  );

  // Count successful failure logs
  const successfulLogs = results.filter((r) => r.status === "fulfilled").length;

  if (successfulLogs > 0) {
    log.info(
      `Failure logged to ${successfulLogs} server(s)`,
      "audit",
    );
  }
}

/**
 * Wrap an async operation with error handling
 *
 * This is a convenience function that wraps an operation with try-catch
 * and uses handleCommandError for error handling.
 *
 * @param operation The async operation to wrap
 * @param context Error handling context
 *
 * @example
 * ```typescript
 * await withErrorHandling(
 *   async () => {
 *     // ... command logic
 *   },
 *   {
 *     operation: "Bootstrap",
 *     component: "bootstrap",
 *     sshManagers,
 *     projectName: config.project,
 *   }
 * );
 * ```
 */
export async function withErrorHandling<T>(
  operation: () => Promise<T>,
  context: ErrorHandlerContext,
): Promise<T | never> {
  try {
    return await operation();
  } catch (error) {
    await handleCommandError(error, context);
    // handleCommandError calls Deno.exit(1) which never returns
    throw new Error("Unreachable");
  }
}
