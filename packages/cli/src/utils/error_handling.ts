/**
 * Registry-specific error class with additional context
 */
export class RegistryError extends Error {
  constructor(
    message: string,
    public code: string,
    public registry?: string,
    public operation?: string,
    public override cause?: Error,
  ) {
    super(message);
    this.name = "RegistryError";
  }

  /**
   * Create a formatted error message with context
   */
  override toString(): string {
    const parts = [this.message];

    if (this.registry) {
      parts.push(`Registry: ${this.registry}`);
    }

    if (this.operation) {
      parts.push(`Operation: ${this.operation}`);
    }

    if (this.cause) {
      parts.push(`Cause: ${this.cause.message}`);
    }

    return parts.join(" | ");
  }
}

/**
 * Registry error codes
 */
export const RegistryErrorCodes = {
  AUTH_FAILED: "AUTH_FAILED",
  INVALID_CREDENTIALS: "INVALID_CREDENTIALS",
  NOT_LOGGED_IN: "NOT_LOGGED_IN",
  REGISTRY_UNREACHABLE: "REGISTRY_UNREACHABLE",
  CONNECTION_TIMEOUT: "CONNECTION_TIMEOUT",
  INVALID_CONFIG: "INVALID_CONFIG",
  REGISTRY_NOT_CONFIGURED: "REGISTRY_NOT_CONFIGURED",
  ENGINE_NOT_FOUND: "ENGINE_NOT_FOUND",
  ENGINE_ERROR: "ENGINE_ERROR",
  LOCAL_REGISTRY_START_FAILED: "LOCAL_REGISTRY_START_FAILED",
  LOCAL_REGISTRY_STOP_FAILED: "LOCAL_REGISTRY_STOP_FAILED",
  PORT_IN_USE: "PORT_IN_USE",
  UNKNOWN_ERROR: "UNKNOWN_ERROR",
  OPERATION_FAILED: "OPERATION_FAILED",
} as const;

export type RegistryErrorCode =
  typeof RegistryErrorCodes[keyof typeof RegistryErrorCodes];

/**
 * Helper function to safely extract error message from unknown error type
 */
export function getErrorMessage(error: unknown): string {
  if (error instanceof Error) {
    return error.message;
  }
  return String(error);
}

/**
 * Handle registry errors with consistent logging and exit behavior
 */
export function handleRegistryError(
  error: unknown,
  operation: string,
  registry?: string,
): never {
  let registryError: RegistryError;

  if (error instanceof RegistryError) {
    registryError = error;
  } else if (error instanceof Error) {
    // Try to map common errors to specific codes
    let code: RegistryErrorCode = RegistryErrorCodes.OPERATION_FAILED;

    if (error.message.toLowerCase().includes("not logged in")) {
      code = RegistryErrorCodes.NOT_LOGGED_IN;
    } else if (error.message.toLowerCase().includes("unauthorized")) {
      code = RegistryErrorCodes.AUTH_FAILED;
    } else if (
      error.message.toLowerCase().includes("connection refused") ||
      error.message.toLowerCase().includes("unreachable")
    ) {
      code = RegistryErrorCodes.REGISTRY_UNREACHABLE;
    } else if (error.message.toLowerCase().includes("timeout")) {
      code = RegistryErrorCodes.CONNECTION_TIMEOUT;
    }

    registryError = new RegistryError(
      error.message,
      code,
      registry,
      operation,
      error,
    );
  } else {
    registryError = new RegistryError(
      getErrorMessage(error),
      RegistryErrorCodes.UNKNOWN_ERROR,
      registry,
      operation,
    );
  }

  import("./logger.ts").then(({ log }) => {
    log.error(registryError.toString(), `registry:${operation}`);
  });

  Deno.exit(1);
}

/**
 * Create a registry error with consistent formatting
 */
export function createRegistryError(
  message: string,
  code: RegistryErrorCode,
  registry?: string,
  operation?: string,
  cause?: Error,
): RegistryError {
  return new RegistryError(message, code, registry, operation, cause);
}

/**
 * Wrap async operations with error handling
 */
export async function withErrorHandling<T>(
  operation: () => Promise<T>,
  operationName: string,
  registry?: string,
): Promise<T> {
  try {
    return await operation();
  } catch (error) {
    handleRegistryError(error, operationName, registry);
  }
}

/**
 * Check if an error indicates the registry is not authenticated
 */
export function isAuthenticationError(error: unknown): boolean {
  if (error instanceof RegistryError) {
    const authCodes: RegistryErrorCode[] = [
      RegistryErrorCodes.AUTH_FAILED,
      RegistryErrorCodes.INVALID_CREDENTIALS,
      RegistryErrorCodes.NOT_LOGGED_IN,
    ];
    return authCodes.includes(error.code as RegistryErrorCode);
  }

  if (error instanceof Error) {
    const message = error.message.toLowerCase();
    return message.includes("unauthorized") ||
      message.includes("not logged in") ||
      message.includes("authentication");
  }

  return false;
}

/**
 * Check if an error indicates a connection problem
 */
export function isConnectionError(error: unknown): boolean {
  if (error instanceof RegistryError) {
    const connectionCodes: RegistryErrorCode[] = [
      RegistryErrorCodes.REGISTRY_UNREACHABLE,
      RegistryErrorCodes.CONNECTION_TIMEOUT,
    ];
    return connectionCodes.includes(error.code as RegistryErrorCode);
  }

  if (error instanceof Error) {
    const message = error.message.toLowerCase();
    return message.includes("connection refused") ||
      message.includes("unreachable") ||
      message.includes("timeout") ||
      message.includes("network");
  }

  return false;
}
