/**
 * Registry type - local (localhost) or remote (external server)
 */
export type RegistryType = "local" | "remote";

/**
 * Registry operation types
 */
export type RegistryOperation =
  | "add"
  | "remove"
  | "login"
  | "logout"
  | "setup"
  | "setup-local"
  | "setup-remote"
  | "list"
  | "use"
  | "status"
  | "authenticate";

/**
 * Registry credentials for authentication
 */
export interface RegistryCredentials {
  username: string;
  password: string;
}

/**
 * Registry configuration entry
 */
export interface RegistryConfig {
  url: string;
  type: RegistryType;
  port?: number;
  username?: string;
  isDefault?: boolean;
  lastLogin?: string;
  metadata?: Record<string, unknown>;
}

/**
 * Registry status information
 */
export interface RegistryStatus {
  available: boolean;
  authenticated: boolean;
  running?: boolean; // For local registries
  containerId?: string; // For local registries
  message?: string;
  lastChecked?: string;
}

/**
 * Registry information
 */
export interface RegistryInfo {
  url: string;
  type: RegistryType;
  port?: number;
  username?: string;
  isDefault: boolean;
  lastLogin?: string;
  status: RegistryStatus;
  metadata?: Record<string, unknown>;
}

/**
 * Registry setup options
 */
export interface RegistrySetupOptions {
  type?: RegistryType;
  port?: number;
  credentials?: RegistryCredentials;
  isDefault?: boolean;
  skipAuthentication?: boolean;
  metadata?: Record<string, unknown>;
}

/**
 * Registry operation result
 */
export interface RegistryOperationResult {
  success: boolean;
  registry: string;
  operation: RegistryOperation;
  message?: string;
  data?: Record<string, unknown>;
  timestamp?: string;
}

/**
 * Authentication result
 */
export interface AuthenticationResult {
  success: boolean;
  registry: string;
  message?: string;
  authenticated?: boolean;
  timestamp?: string;
}

/**
 * Registry command options
 */
export interface RegistryCommandOptions {
  registry?: string;
  type?: RegistryType;
  port?: number;
  username?: string;
  password?: string;
  default?: boolean;
  skipAuth?: boolean;
  force?: boolean;
  verbose?: boolean;
}

/**
 * Registry event types
 */
export type RegistryEventType =
  | "registry.added"
  | "registry.removed"
  | "registry.authenticated"
  | "registry.failed"
  | "registry.default_changed"
  | "local_registry.started"
  | "local_registry.stopped"
  | "local_registry.error";

/**
 * Registry event
 */
export interface RegistryEvent {
  type: RegistryEventType;
  registry: string;
  timestamp: string;
  data?: Record<string, unknown>;
  error?: string;
}
