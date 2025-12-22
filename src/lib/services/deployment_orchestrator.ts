/**
 * Deployment Orchestrator Service
 *
 * Manages the complete deployment workflow including:
 * - Proxy installation and configuration
 * - Service container deployment with zero-downtime rollout
 * - Health checks and rollback handling
 * - Old container cleanup after successful deployments
 * - Deployment metrics collection and reporting
 */

import type { Configuration } from "../configuration.ts";
import type { ServiceConfiguration } from "../configuration/service.ts";
import type { SSHManager } from "../../utils/ssh.ts";
import {
  ContainerDeploymentService,
  type DeploymentResult,
} from "./container_deployment_service.ts";
import {
  type ProxyConfigResult,
  type ProxyInstallResult,
  ProxyService,
} from "./proxy_service.ts";
import {
  type DeploymentMetrics,
  deploymentMetrics,
} from "./deployment_metrics.ts";
import { log } from "../../utils/logger.ts";

export interface OrchestrationOptions {
  version?: string;
  allSshManagers?: SSHManager[];
}

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
 * Orchestrates the complete deployment workflow
 */
export class DeploymentOrchestrator {
  private config: Configuration;
  private sshManagers: SSHManager[];
  private deploymentService: ContainerDeploymentService;
  private proxyService: ProxyService;

  constructor(config: Configuration, sshManagers: SSHManager[]) {
    this.config = config;
    this.sshManagers = sshManagers;

    this.deploymentService = new ContainerDeploymentService(
      config.builder.engine,
      config,
    );

    this.proxyService = new ProxyService(
      config.builder.engine,
      config,
      sshManagers,
    );
  }

  /**
   * Execute the complete deployment orchestration
   */
  async orchestrateDeployment(
    services: ServiceConfiguration[],
    targetHosts: string[],
    options: OrchestrationOptions = {},
  ): Promise<OrchestrationResult> {
    // Start metrics collection
    const deploymentId = deploymentMetrics.startDeployment(
      this.config.project,
      options.version,
      services.length,
    );

    const result: OrchestrationResult = {
      success: false,
      proxyInstallResults: [],
      deploymentResults: [],
      proxyConfigResults: [],
      errors: [],
      warnings: [],
      deploymentId,
    };

    try {
      // Step 1: Install proxy on hosts that need it
      const servicesWithProxy = services.filter((s) => s.proxy?.enabled);
      if (servicesWithProxy.length > 0) {
        deploymentMetrics.startStep(deploymentId, "proxy_installation");
        result.proxyInstallResults = await this.installProxyOnHosts(
          servicesWithProxy,
          targetHosts,
        );
        deploymentMetrics.finishStep(deploymentId, "proxy_installation", true);
        deploymentMetrics.recordProxyInstalls(
          deploymentId,
          result.proxyInstallResults,
        );

        // Check for proxy installation failures
        const failedInstalls = result.proxyInstallResults.filter((r) =>
          !r.success
        );
        if (failedInstalls.length > 0) {
          result.errors.push(
            `Proxy installation failed on ${failedInstalls.length} host(s): ${
              failedInstalls.map((f) => `${f.host} (${f.error})`).join(", ")
            }`,
          );
        }
      }

      // Step 2: Deploy service containers
      if (services.length > 0) {
        deploymentMetrics.startStep(deploymentId, "container_deployment");
        result.deploymentResults = await this.deployServiceContainers(
          services,
          targetHosts,
          options,
        );
        deploymentMetrics.finishStep(
          deploymentId,
          "container_deployment",
          true,
        );
        deploymentMetrics.recordDeployments(
          deploymentId,
          result.deploymentResults,
        );

        // Check for deployment failures
        const failedDeployments = result.deploymentResults.filter((r) =>
          !r.success
        );
        if (failedDeployments.length > 0) {
          result.errors.push(
            `Container deployment failed for ${failedDeployments.length} service(s): ${
              failedDeployments.map((f) =>
                `${f.service}@${f.host} (${f.error})`
              ).join(", ")
            }`,
          );
        }
      }

      // Step 3: Configure proxy for services and handle health checks
      if (
        servicesWithProxy.length > 0 &&
        result.deploymentResults.some((r) => r.success)
      ) {
        deploymentMetrics.startStep(deploymentId, "proxy_configuration");
        const healthCheckResult = await this.configureProxyAndHealthChecks(
          servicesWithProxy,
          result.deploymentResults,
          deploymentId,
        );
        deploymentMetrics.finishStep(deploymentId, "proxy_configuration", true);
        deploymentMetrics.recordProxyConfigs(
          deploymentId,
          healthCheckResult.proxyConfigResults,
        );

        result.proxyConfigResults = healthCheckResult.proxyConfigResults;
        result.errors.push(...healthCheckResult.errors);
        result.warnings.push(...healthCheckResult.warnings);
      }

      // Determine overall success
      result.success = result.errors.length === 0;

      // Finish metrics collection
      const metrics = deploymentMetrics.finishDeployment(
        deploymentId,
        result.success,
      );
      result.metrics = metrics;

      return result;
    } catch (error) {
      const errorMessage = error instanceof Error
        ? error.message
        : String(error);
      result.errors.push(`Orchestration failed: ${errorMessage}`);
      result.success = false;

      log.error(`Deployment orchestration failed: ${errorMessage}`, "deploy");
      return result;
    }
  }

  /**
   * Install proxy on hosts that need it
   */
  private async installProxyOnHosts(
    servicesWithProxy: ServiceConfiguration[],
    targetHosts: string[],
  ): Promise<ProxyInstallResult[]> {
    log.info(
      `Found ${servicesWithProxy.length} service(s) with proxy configuration`,
      "proxy",
    );

    const proxyHosts = ProxyService.getHostsNeedingProxy(
      servicesWithProxy,
      targetHosts,
    );

    if (proxyHosts.size === 0) {
      log.info("No hosts require proxy installation", "proxy");
      return [];
    }

    return await this.proxyService.ensureProxyOnHosts(proxyHosts);
  }

  /**
   * Deploy service containers using the deployment service
   */
  private async deployServiceContainers(
    services: ServiceConfiguration[],
    targetHosts: string[],
    options: OrchestrationOptions,
  ): Promise<DeploymentResult[]> {
    return await this.deploymentService.deployServices(
      services,
      this.sshManagers,
      targetHosts,
      {
        version: options.version,
        allSshManagers: options.allSshManagers,
      },
    );
  }

  /**
   * Configure proxy for services and handle health checks with rollback
   */
  private async configureProxyAndHealthChecks(
    servicesWithProxy: ServiceConfiguration[],
    deploymentResults: DeploymentResult[],
    deploymentId: string,
  ): Promise<{
    proxyConfigResults: ProxyConfigResult[];
    errors: string[];
    warnings: string[];
  }> {
    const configResults: ProxyConfigResult[] = [];
    const errors: string[] = [];
    const warnings: string[] = [];

    await log.group("Service Proxy Configuration & Health Checks", async () => {
      // Configure proxy for each service
      const proxyConfigResults = await this.proxyService
        .configureProxyForServices(
          servicesWithProxy,
        );
      configResults.push(...proxyConfigResults);

      // Track proxy configuration failures
      const failedConfigs = proxyConfigResults.filter((r) => !r.success);
      if (failedConfigs.length > 0) {
        errors.push(
          `Proxy configuration failed for ${failedConfigs.length} service(s): ${
            failedConfigs.map((f) => `${f.service}@${f.host} (${f.error})`)
              .join(", ")
          }`,
        );
      }

      // Wait for health checks and cleanup old containers
      await this.handleHealthChecksAndCleanup(
        servicesWithProxy,
        deploymentResults,
        deploymentId,
        errors,
        warnings,
      );
    });

    return { proxyConfigResults: configResults, errors, warnings };
  }

  /**
   * Handle health checks and cleanup/rollback operations
   */
  private async handleHealthChecksAndCleanup(
    servicesWithProxy: ServiceConfiguration[],
    deploymentResults: DeploymentResult[],
    deploymentId: string,
    errors: string[],
    warnings: string[],
  ): Promise<void> {
    for (const result of deploymentResults) {
      if (!result.success || !result.oldContainerName) {
        continue; // Skip failed deployments or deployments without old containers
      }

      const service = servicesWithProxy.find((s) => s.name === result.service);
      if (!service || !service.proxy?.enabled) {
        continue; // Skip services without proxy
      }

      const hostSsh = this.sshManagers.find((ssh) =>
        ssh.getHost() === result.host
      );
      if (!hostSsh) {
        warnings.push(
          `Could not find SSH connection for ${result.service}@${result.host} cleanup`,
        );
        continue;
      }

      try {
        // Wait for service to become healthy
        const healthCheckStart = Date.now();
        const isHealthy = await this.proxyService.waitForServiceHealthy(
          service,
          result.host,
          hostSsh,
        );
        const healthCheckDuration = Date.now() - healthCheckStart;

        // Record health check metrics
        deploymentMetrics.recordHealthCheck(
          deploymentId,
          service.name,
          result.host,
          isHealthy,
          healthCheckDuration,
          !isHealthy ? "Health check timed out or failed" : undefined,
        );

        if (isHealthy) {
          // Clean up old container after health checks pass
          await this.deploymentService.cleanupOldContainer(
            result.oldContainerName,
            result.host,
            hostSsh,
          );

          log.success(
            `Successfully cleaned up old container for ${service.name}@${result.host}`,
            "deploy",
          );
        } else {
          // Health checks failed - rollback
          errors.push(
            `Health check failed for ${service.name}@${result.host} - rolling back to previous version`,
          );
          await this.performRollback(
            service,
            result,
            hostSsh,
            deploymentId,
            errors,
          );
        }
      } catch (error) {
        const errorMessage = error instanceof Error
          ? error.message
          : String(error);
        errors.push(
          `Health check or cleanup failed for ${service.name}@${result.host}: ${errorMessage}`,
        );
      }
    }
  }

  /**
   * Perform rollback by removing new container and restoring old one
   */
  private async performRollback(
    service: ServiceConfiguration,
    result: DeploymentResult,
    hostSsh: SSHManager,
    deploymentId: string,
    errors: string[],
  ): Promise<void> {
    log.error(
      `Health checks failed for ${service.name} on ${result.host}, rolling back...`,
      "deploy",
    );

    try {
      // Remove new container that failed health checks
      if (result.containerName) {
        await this.deploymentService.cleanupOldContainer(
          result.containerName,
          result.host,
          hostSsh,
        );
      }

      // Rename old container back to original name
      if (result.oldContainerName && result.containerName) {
        const renameCmd =
          `${this.config.builder.engine} rename ${result.oldContainerName} ${result.containerName}`;
        const renameResult = await hostSsh.executeCommand(renameCmd);

        if (!renameResult.success) {
          throw new Error(`Failed to rename container: ${renameResult.stderr}`);
        }
      }

      log.info(
        `Rollback complete: ${service.name} on ${result.host} restored to previous version`,
        "deploy",
      );

      // Record successful rollback
      deploymentMetrics.recordRollback(
        deploymentId,
        service.name,
        result.host,
        true,
      );
    } catch (rollbackError) {
      const rollbackErrorMessage = rollbackError instanceof Error
        ? rollbackError.message
        : String(rollbackError);

      errors.push(
        `Rollback failed for ${service.name}@${result.host}: ${rollbackErrorMessage}`,
      );

      log.error(
        `Rollback failed for ${service.name}@${result.host}: ${rollbackErrorMessage}`,
        "deploy",
      );

      // Record failed rollback
      deploymentMetrics.recordRollback(
        deploymentId,
        service.name,
        result.host,
        false,
        rollbackErrorMessage,
      );
    }
  }

  /**
   * Get deployment summary for logging/reporting
   */
  getDeploymentSummary(result: OrchestrationResult): {
    totalServices: number;
    successfulDeployments: number;
    failedDeployments: number;
    proxyInstallations: number;
    proxyConfigurations: number;
    hasErrors: boolean;
    hasWarnings: boolean;
  } {
    return {
      totalServices: result.deploymentResults.length,
      successfulDeployments:
        result.deploymentResults.filter((r) => r.success).length,
      failedDeployments:
        result.deploymentResults.filter((r) => !r.success).length,
      proxyInstallations:
        result.proxyInstallResults.filter((r) => r.success).length,
      proxyConfigurations:
        result.proxyConfigResults.filter((r) => r.success).length,
      hasErrors: result.errors.length > 0,
      hasWarnings: result.warnings.length > 0,
    };
  }

  /**
   * Log deployment results in a structured way
   */
  logDeploymentSummary(result: OrchestrationResult): void {
    const summary = this.getDeploymentSummary(result);

    if (result.success) {
      log.success(
        `Deployment completed successfully: ${summary.successfulDeployments}/${summary.totalServices} services deployed`,
        "deploy",
      );
    } else {
      log.error(
        `Deployment completed with errors: ${summary.successfulDeployments}/${summary.totalServices} services deployed`,
        "deploy",
      );
    }

    if (summary.proxyInstallations > 0) {
      log.info(
        `Proxy installed on ${summary.proxyInstallations} host(s)`,
        "deploy",
      );
    }

    if (summary.proxyConfigurations > 0) {
      log.info(
        `Proxy configured for ${summary.proxyConfigurations} service(s)`,
        "deploy",
      );
    }

    // Log errors
    for (const error of result.errors) {
      log.error(error, "deploy");
    }

    // Log warnings
    for (const warning of result.warnings) {
      log.warn(warning, "deploy");
    }

    // Log detailed metrics summary if available
    if (result.deploymentId && result.metrics) {
      const metricsReport = deploymentMetrics.generateSummary(
        result.deploymentId,
      );
      if (metricsReport) {
        log.info(metricsReport, "metrics");
      }
    }
  }
}
