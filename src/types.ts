/**
 * Central type definitions and re-exports for the Jiji application
 */

// ============================================================================
// Global Options and Command Types
// ============================================================================

export interface GlobalOptions {
  environment?: string;
  verbose?: boolean;
  quiet?: boolean;
  version?: string;
  configFile?: string;
  hosts?: string;
  services?: string;
}

// ============================================================================
// Configuration System Types
// ============================================================================

export interface ValidationError {
  path: string;
  message: string;
  code?: string;
}

export interface ValidationWarning {
  path: string;
  message: string;
  code?: string;
}

export interface ValidationResult {
  valid: boolean;
  errors: ValidationError[];
  warnings: ValidationWarning[];
}

export interface ConfigurationContext {
  environment?: string;
  configPath?: string;
  [key: string]: unknown;
}

// Re-export configuration types
export type { ContainerEngine } from "./lib/configuration.ts";
export type { Configuration } from "./lib/configuration.ts";
export type { ServiceConfiguration } from "./lib/configuration.ts";
export type { SSHConfiguration } from "./lib/configuration.ts";
export type { EnvironmentConfiguration } from "./lib/configuration.ts";
export { ConfigurationError } from "./lib/configuration.ts";

// ============================================================================
// Common Utility Types
// ============================================================================

export type {
  AggregatedResults,
  CommandContext,
  CommandContextOptions,
  CommandHandler,
  ConfigLoadResult,
  EngineInstallResult,
  ErrorHandlerContext,
  GitFileStatus,
  KamalProxyDeployOptions,
  LoggerOptions,
  LogLevel,
  OperationResult,
  ParsedMount,
  ServiceFilterOptions,
  ServiceGroupingOptions,
  VersionOptions,
} from "./types/common.ts";

// ============================================================================
// SSH and Remote Execution Types
// ============================================================================

export type { CommandResult, SSHConnectionConfig } from "./types/ssh.ts";

export { SSH_ALGORITHMS } from "./types/ssh.ts";

// ============================================================================
// Audit and Logging Types
// ============================================================================

export type {
  AuditEntry,
  LockInfo,
  LockManager,
  RemoteAuditResult,
} from "./types/audit.ts";

// ============================================================================
// Deployment Types
// ============================================================================

export type {
  BuildResult,
  BuildServiceOptions,
  DeploymentMetrics,
  DeploymentOptions,
  DeploymentResult,
  OrchestrationOptions,
  OrchestrationResult,
  ProxyConfigResult,
  ProxyInstallResult,
  PruneOptions,
  PruneResult,
  PushOptions,
  PushResult,
  RemoteAuthResult,
} from "./types/deployment.ts";

// ============================================================================
// Registry Types
// ============================================================================

export type {
  AuthenticationResult,
  LocalRegistryContainer,
  RegistryBackupInfo,
  RegistryCommandOptions,
  RegistryConfig,
  RegistryCredentials,
  RegistryEnvironment,
  RegistryEvent,
  RegistryEventType,
  RegistryHealthCheck,
  RegistryImageInfo,
  RegistryInfo,
  RegistryListOptions,
  RegistryMetrics,
  RegistryMigrationOptions,
  RegistryOperation,
  RegistryOperationResult,
  RegistrySearchResult,
  RegistryServiceConfig,
  RegistrySetupOptions,
  RegistryStatus,
  RegistryType,
  RegistryUrlComponents,
  RegistryValidationResult,
  RegistryValidationRules,
} from "./types/registry.ts";

// ============================================================================
// Network Types
// ============================================================================

export type {
  ContainerRegistration,
  CorrosionConfig,
  DNSConfig,
  NetworkDependencies,
  NetworkDiscovery,
  NetworkServer,
  NetworkSetupResult,
  NetworkStatus,
  NetworkTopology,
  ServerRegistration,
  ServiceRegistration,
  WireGuardConfig,
  WireGuardPeer,
} from "./types/network.ts";

// ============================================================================
// Logs Types
// ============================================================================

export type {
  BuildLogsCommandOptions,
  FetchLogsOptions,
  FollowLogsOptions,
  LogsOptions,
} from "./types/logs.ts";

// ============================================================================
// Error Handling Types
// ============================================================================

export {
  RegistryError,
  type RegistryErrorCode,
  RegistryErrorCodes,
} from "./utils/error_handling.ts";
