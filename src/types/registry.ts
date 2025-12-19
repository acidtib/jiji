import type { ContainerEngine } from "../lib/configuration/builder.ts";

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
 * Comprehensive registry information
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
 * Registry service configuration
 */
export interface RegistryServiceConfig {
  engine: ContainerEngine;
  configPath?: string;
  defaultPort?: number;
  timeout?: number;
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
 * Registry list options
 */
export interface RegistryListOptions {
  type?: RegistryType;
  showStatus?: boolean;
  format?: "table" | "json" | "yaml";
}

/**
 * Registry validation result
 */
export interface RegistryValidationResult {
  valid: boolean;
  errors: string[];
  warnings: string[];
}

/**
 * Registry URL components
 */
export interface RegistryUrlComponents {
  hostname: string;
  port?: number;
  protocol?: string;
  path?: string;
}

/**
 * Local registry container information
 */
export interface LocalRegistryContainer {
  containerId: string;
  name: string;
  port: number;
  status: "running" | "stopped" | "error";
  image: string;
  created: string;
}

/**
 * Registry health check result
 */
export interface RegistryHealthCheck {
  healthy: boolean;
  registry: string;
  responseTime?: number;
  error?: string;
  timestamp: string;
}

/**
 * Registry metrics
 */
export interface RegistryMetrics {
  registry: string;
  type: RegistryType;
  totalImages?: number;
  storageUsed?: string;
  uptime?: string;
  lastActivity?: string;
}

/**
 * Registry search result
 */
export interface RegistrySearchResult {
  name: string;
  description?: string;
  tags: string[];
  lastUpdated?: string;
  size?: string;
}

/**
 * Registry image information
 */
export interface RegistryImageInfo {
  name: string;
  tag: string;
  digest?: string;
  size?: string;
  created?: string;
  registry: string;
}

/**
 * Registry backup information
 */
export interface RegistryBackupInfo {
  registry: string;
  backupPath: string;
  timestamp: string;
  size?: string;
  compressed?: boolean;
}

/**
 * Registry migration options
 */
export interface RegistryMigrationOptions {
  sourceRegistry: string;
  targetRegistry: string;
  includeImages?: boolean;
  includeConfig?: boolean;
  dryRun?: boolean;
  force?: boolean;
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

/**
 * Registry configuration validation rules
 */
export interface RegistryValidationRules {
  requireUsername?: boolean;
  requirePassword?: boolean;
  allowedPorts?: number[];
  allowedHostnames?: string[];
  maxRegistries?: number;
}

/**
 * Registry environment variables
 */
export interface RegistryEnvironment {
  REGISTRY_USERNAME?: string;
  REGISTRY_PASSWORD?: string;
  REGISTRY_URL?: string;
  REGISTRY_PORT?: string;
  REGISTRY_TYPE?: RegistryType;
}
