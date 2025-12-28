/**
 * Deployment and service-related type definitions
 */

import type { ContainerEngine } from "../lib/configuration/builder.ts";
import type { RegistryConfiguration } from "../lib/configuration/registry.ts";
import type { SSHManager } from "../utils/ssh.ts";

/**
 * Build service options
 */
export interface BuildServiceOptions {
  engine: ContainerEngine;
  registry: RegistryConfiguration;
  globalOptions: {
    environment?: string;
    verbose?: boolean;
    version?: string;
    configFile?: string;
    hosts?: string;
    services?: string;
  };
  noCache?: boolean;
  push?: boolean;
  cacheEnabled?: boolean;
}

/**
 * Build result
 */
export interface BuildResult {
  serviceName: string;
  success: boolean;
  imageName: string;
  latestImageName: string;
  error?: string;
}

/**
 * Deployment options for container deployment
 */
export interface DeploymentOptions {
  /**
   * Custom version tag (defaults to "latest")
   */
  version?: string;
  /**
   * All SSH managers for cluster-wide operations
   */
  allSshManagers?: SSHManager[];
  /**
   * Server configuration (includes optional alias)
   */
  serverConfig?: {
    host: string;
    arch?: string;
    alias?: string;
  };
  /**
   * Whether this service has multiple servers (determines if instance ID should be set)
   */
  hasMultipleServers?: boolean;
}

/**
 * Container deployment result
 */
export interface DeploymentResult {
  service: string;
  host: string;
  success: boolean;
  containerName?: string;
  imageName?: string;
  containerIp?: string;
  oldContainerName?: string; // Name of the old container that was renamed (for cleanup after health checks)
  error?: string;
}

/**
 * Deployment metrics data
 */
export interface DeploymentMetrics {
  deploymentId: string;
  timestamp: Date;
  projectName: string;
  version?: string;

  // Timing metrics
  startTime: Date;
  endTime?: Date;
  totalDurationMs?: number;

  // Service metrics
  totalServices: number;
  successfulDeployments: number;
  failedDeployments: number;
  rolledBackDeployments: number;

  // Proxy metrics
  proxyHostsConfigured: number;
  proxyServicesConfigured: number;
  proxyInstallFailures: number;
  proxyConfigFailures: number;

  // Health check metrics
  healthChecksPassed: number;
  healthChecksFailed: number;
  avgHealthCheckDurationMs?: number;

  // Rollback metrics
  rollbacksTriggered: number;
  rollbacksSuccessful: number;
  rollbacksFailed: number;

  // Error tracking
  errors: Array<{
    type: "deployment" | "proxy" | "health_check" | "rollback" | "other";
    service?: string;
    host?: string;
    message: string;
    timestamp: Date;
  }>;

  // Performance metrics
  deploymentSteps: Array<{
    step: string;
    startTime: Date;
    endTime?: Date;
    durationMs?: number;
    success: boolean;
    error?: string;
  }>;
}

/**
 * Orchestration options for multi-service deployment
 */
export interface OrchestrationOptions {
  version?: string;
  allSshManagers?: SSHManager[];
}

/**
 * Orchestration result
 */
export interface OrchestrationResult {
  success: boolean;
  proxyInstallResults: ProxyInstallResult[];
  deploymentResults: DeploymentResult[];
  proxyConfigResults: ProxyConfigResult[];
  errors: string[];
  warnings: string[];
  deploymentId?: string;
  metrics?: DeploymentMetrics;
}

/**
 * Image prune options
 */
export interface PruneOptions {
  /**
   * Number of recent images to retain per service (default: 3)
   */
  retain?: number;
  /**
   * Whether to remove dangling images (default: true)
   */
  removeDangling?: boolean;
}

/**
 * Image prune result
 */
export interface PruneResult {
  host: string;
  success: boolean;
  imagesRemoved: number;
  spaceSaved?: string;
  error?: string;
}

/**
 * Image push options
 */
export interface PushOptions {
  engine: ContainerEngine;
  registry: RegistryConfiguration;
  globalOptions: {
    environment?: string;
    verbose?: boolean;
    version?: string;
    configFile?: string;
    hosts?: string;
    services?: string;
  };
}

/**
 * Image push result
 */
export interface PushResult {
  imageName: string;
  success: boolean;
  error?: Error;
}

/**
 * Proxy installation result
 */
export interface ProxyInstallResult {
  host: string;
  success: boolean;
  message?: string;
  error?: string;
  version?: string;
}

/**
 * Proxy configuration result
 */
export interface ProxyConfigResult {
  service: string;
  host: string;
  success: boolean;
  message?: string;
  error?: string;
}

/**
 * Remote registry authentication result
 */
export interface RemoteAuthResult {
  host: string;
  success: boolean;
  error?: string;
}
