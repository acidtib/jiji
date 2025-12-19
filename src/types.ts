export interface AuditEntry {
  timestamp: string;
  action: string;
  details?: Record<string, unknown>;
  user?: string;
  host?: string;
  status: "started" | "success" | "failed" | "warning";
  message?: string;
}

export interface GlobalOptions {
  environment?: string;
  verbose?: boolean;
  version?: string;
  configFile?: string;
  hosts?: string;
  services?: string;
}

// Configuration system types
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

// Re-export types from configuration system
export type { ContainerEngine } from "./lib/configuration.ts";
export type { Configuration } from "./lib/configuration.ts";
export type { ServiceConfiguration } from "./lib/configuration.ts";
export type { SSHConfiguration } from "./lib/configuration.ts";
export type { EnvironmentConfiguration } from "./lib/configuration.ts";
export { ConfigurationError } from "./lib/configuration.ts";

// Re-export registry types
export type {
  AuthenticationResult,
  RegistryCommandOptions,
  RegistryCredentials,
  RegistryHealthCheck,
  RegistryInfo,
  RegistryListOptions,
  RegistryOperation,
  RegistryOperationResult,
  RegistrySetupOptions,
  RegistryStatus,
  RegistryType,
  RegistryValidationResult,
} from "./types/registry.ts";

// Re-export error handling types
export {
  RegistryError,
  type RegistryErrorCode,
  RegistryErrorCodes,
} from "./utils/error_handling.ts";
