/**
 * Deployment Metrics Service
 *
 * Provides observability and metrics collection for deployment operations
 */

import { log } from "../../utils/logger.ts";
import type {
  DeploymentMetrics,
  DeploymentResult,
  ProxyConfigResult,
  ProxyInstallResult,
} from "../../types.ts";

/**
 * Deployment step timing helper
 */
export class DeploymentStepTimer {
  private startTime: Date;
  private stepName: string;

  constructor(stepName: string) {
    this.stepName = stepName;
    this.startTime = new Date();
  }

  finish(success: boolean, error?: string): {
    step: string;
    startTime: Date;
    endTime: Date;
    durationMs: number;
    success: boolean;
    error?: string;
  } {
    const endTime = new Date();
    const durationMs = endTime.getTime() - this.startTime.getTime();

    return {
      step: this.stepName,
      startTime: this.startTime,
      endTime,
      durationMs,
      success,
      error,
    };
  }
}

/**
 * Service for collecting and analyzing deployment metrics
 */
export class DeploymentMetricsService {
  private metrics: Map<string, DeploymentMetrics> = new Map();
  private activeTimers: Map<string, DeploymentStepTimer> = new Map();

  /**
   * Start tracking a new deployment
   */
  startDeployment(
    projectName: string,
    version?: string,
    totalServices: number = 0,
  ): string {
    const deploymentId = `${projectName}-${Date.now()}-${
      Math.random().toString(36).substr(2, 9)
    }`;

    const metrics: DeploymentMetrics = {
      deploymentId,
      timestamp: new Date(),
      projectName,
      version,
      startTime: new Date(),
      totalServices,
      successfulDeployments: 0,
      failedDeployments: 0,
      rolledBackDeployments: 0,
      proxyHostsConfigured: 0,
      proxyServicesConfigured: 0,
      proxyInstallFailures: 0,
      proxyConfigFailures: 0,
      healthChecksPassed: 0,
      healthChecksFailed: 0,
      rollbacksTriggered: 0,
      rollbacksSuccessful: 0,
      rollbacksFailed: 0,
      errors: [],
      deploymentSteps: [],
    };

    this.metrics.set(deploymentId, metrics);

    log.debug(
      `Started deployment tracking: ${deploymentId} (${projectName} v${version})`,
      "metrics",
    );

    return deploymentId;
  }

  /**
   * Start timing a deployment step
   */
  startStep(deploymentId: string, stepName: string): void {
    const timerKey = `${deploymentId}:${stepName}`;
    this.activeTimers.set(timerKey, new DeploymentStepTimer(stepName));

    log.debug(`Started step: ${stepName}`, "metrics");
  }

  /**
   * Finish timing a deployment step
   */
  finishStep(
    deploymentId: string,
    stepName: string,
    success: boolean,
    error?: string,
  ): void {
    const timerKey = `${deploymentId}:${stepName}`;
    const timer = this.activeTimers.get(timerKey);

    if (!timer) {
      log.warn(`No timer found for step: ${stepName}`, "metrics");
      return;
    }

    const stepMetrics = timer.finish(success, error);
    this.activeTimers.delete(timerKey);

    const metrics = this.metrics.get(deploymentId);
    if (metrics) {
      metrics.deploymentSteps.push(stepMetrics);

      log.debug(
        `Finished step: ${stepName} (${stepMetrics.durationMs}ms, success: ${success})`,
        "metrics",
      );
    }
  }

  /**
   * Record proxy installation results
   */
  recordProxyInstalls(
    deploymentId: string,
    results: ProxyInstallResult[],
  ): void {
    const metrics = this.metrics.get(deploymentId);
    if (!metrics) return;

    for (const result of results) {
      if (result.success) {
        metrics.proxyHostsConfigured++;
      } else {
        metrics.proxyInstallFailures++;
        metrics.errors.push({
          type: "proxy",
          host: result.host || "unknown",
          message: result.error || "Proxy installation failed",
          timestamp: new Date(),
        });
      }
    }

    log.debug(
      `Recorded proxy installs: ${results.length} total, ${metrics.proxyInstallFailures} failures`,
      "metrics",
    );
  }

  /**
   * Record service deployment results
   */
  recordDeployments(
    deploymentId: string,
    results: DeploymentResult[],
  ): void {
    const metrics = this.metrics.get(deploymentId);
    if (!metrics) return;

    for (const result of results) {
      if (result.success) {
        metrics.successfulDeployments++;
      } else {
        metrics.failedDeployments++;
        metrics.errors.push({
          type: "deployment",
          service: result.service || "unknown",
          host: result.host || "unknown",
          message: result.error || "Deployment failed",
          timestamp: new Date(),
        });
      }
    }

    log.debug(
      `Recorded deployments: ${metrics.successfulDeployments} successful, ${metrics.failedDeployments} failed`,
      "metrics",
    );
  }

  /**
   * Record proxy configuration results
   */
  recordProxyConfigs(
    deploymentId: string,
    results: ProxyConfigResult[],
  ): void {
    const metrics = this.metrics.get(deploymentId);
    if (!metrics) return;

    for (const result of results) {
      if (result.success) {
        metrics.proxyServicesConfigured++;
      } else {
        metrics.proxyConfigFailures++;
        metrics.errors.push({
          type: "proxy",
          service: result.service || "unknown",
          host: result.host || "unknown",
          message: result.error || "Proxy configuration failed",
          timestamp: new Date(),
        });
      }
    }

    log.debug(
      `Recorded proxy configs: ${metrics.proxyServicesConfigured} successful, ${metrics.proxyConfigFailures} failed`,
      "metrics",
    );
  }

  /**
   * Record health check result
   */
  recordHealthCheck(
    deploymentId: string,
    service: string,
    host: string,
    passed: boolean,
    durationMs: number,
    error?: string,
  ): void {
    const metrics = this.metrics.get(deploymentId);
    if (!metrics) return;

    if (passed) {
      metrics.healthChecksPassed++;
    } else {
      metrics.healthChecksFailed++;
      metrics.errors.push({
        type: "health_check",
        service,
        host,
        message: error || "Health check failed",
        timestamp: new Date(),
      });
    }

    // Update average health check duration
    const totalChecks = metrics.healthChecksPassed + metrics.healthChecksFailed;
    const currentAvg = metrics.avgHealthCheckDurationMs || 0;
    metrics.avgHealthCheckDurationMs =
      (currentAvg * (totalChecks - 1) + durationMs) / totalChecks;

    log.debug(
      `Recorded health check: ${service}@${host} (${durationMs}ms, passed: ${passed})`,
      "metrics",
    );
  }

  /**
   * Record rollback event
   */
  recordRollback(
    deploymentId: string,
    service: string,
    host: string,
    successful: boolean,
    error?: string,
  ): void {
    const metrics = this.metrics.get(deploymentId);
    if (!metrics) return;

    metrics.rollbacksTriggered++;

    if (successful) {
      metrics.rollbacksSuccessful++;
      metrics.rolledBackDeployments++;
    } else {
      metrics.rollbacksFailed++;
      metrics.errors.push({
        type: "rollback",
        service,
        host,
        message: error || "Rollback failed",
        timestamp: new Date(),
      });
    }

    log.debug(
      `Recorded rollback: ${service}@${host} (successful: ${successful})`,
      "metrics",
    );
  }

  /**
   * Finish deployment tracking
   */
  finishDeployment(
    deploymentId: string,
    success: boolean,
  ): DeploymentMetrics | undefined {
    const metrics = this.metrics.get(deploymentId);
    if (!metrics) return undefined;

    metrics.endTime = new Date();
    metrics.totalDurationMs = metrics.endTime.getTime() -
      metrics.startTime.getTime();

    // Clean up any remaining timers for this deployment
    for (const [timerKey, timer] of this.activeTimers) {
      if (timerKey.startsWith(`${deploymentId}:`)) {
        const stepMetrics = timer.finish(
          false,
          "Deployment ended before step completion",
        );
        metrics.deploymentSteps.push(stepMetrics);
        this.activeTimers.delete(timerKey);
      }
    }

    log.say(
      `- Deployment ${deploymentId} finished: ${
        success ? "SUCCESS" : "FAILURE"
      } (${metrics.totalDurationMs}ms)`,
      0,
    );

    return metrics;
  }

  /**
   * Get metrics for a deployment
   */
  getMetrics(deploymentId: string): DeploymentMetrics | undefined {
    return this.metrics.get(deploymentId);
  }

  /**
   * Get all recorded metrics
   */
  getAllMetrics(): DeploymentMetrics[] {
    return Array.from(this.metrics.values());
  }

  /**
   * Generate deployment summary report
   */
  generateSummary(deploymentId: string): string | undefined {
    const metrics = this.metrics.get(deploymentId);
    if (!metrics) return undefined;

    const lines: string[] = [];
    lines.push(`\n=== Deployment Summary: ${metrics.deploymentId} ===`);
    lines.push(`Project: ${metrics.projectName}`);
    if (metrics.version) lines.push(`Version: ${metrics.version}`);
    lines.push(`Started: ${metrics.startTime.toISOString()}`);
    if (metrics.endTime) {
      lines.push(`Finished: ${metrics.endTime.toISOString()}`);
    }
    if (metrics.totalDurationMs) {
      lines.push(`Duration: ${(metrics.totalDurationMs / 1000).toFixed(2)}s`);
    }

    lines.push("\n--- Service Deployments ---");
    lines.push(`Total Services: ${metrics.totalServices}`);
    lines.push(`Successful: ${metrics.successfulDeployments}`);
    lines.push(`Failed: ${metrics.failedDeployments}`);
    lines.push(`Rolled Back: ${metrics.rolledBackDeployments}`);

    if (
      metrics.proxyHostsConfigured > 0 || metrics.proxyServicesConfigured > 0
    ) {
      lines.push("\n--- Proxy Configuration ---");
      lines.push(`Hosts Configured: ${metrics.proxyHostsConfigured}`);
      lines.push(`Services Configured: ${metrics.proxyServicesConfigured}`);
      lines.push(`Install Failures: ${metrics.proxyInstallFailures}`);
      lines.push(`Config Failures: ${metrics.proxyConfigFailures}`);
    }

    if (metrics.healthChecksPassed > 0 || metrics.healthChecksFailed > 0) {
      lines.push("\n--- Health Checks ---");
      lines.push(`Passed: ${metrics.healthChecksPassed}`);
      lines.push(`Failed: ${metrics.healthChecksFailed}`);
      if (metrics.avgHealthCheckDurationMs) {
        lines.push(
          `Avg Duration: ${metrics.avgHealthCheckDurationMs.toFixed(0)}ms`,
        );
      }
    }

    if (metrics.rollbacksTriggered > 0) {
      lines.push("\n--- Rollbacks ---");
      lines.push(`Triggered: ${metrics.rollbacksTriggered}`);
      lines.push(`Successful: ${metrics.rollbacksSuccessful}`);
      lines.push(`Failed: ${metrics.rollbacksFailed}`);
    }

    if (metrics.deploymentSteps.length > 0) {
      lines.push("\n--- Step Timing ---");
      for (const step of metrics.deploymentSteps) {
        const status = step.success ? "OK" : "FAILED";
        const duration = step.durationMs
          ? `${(step.durationMs / 1000).toFixed(2)}s`
          : "N/A";
        lines.push(`  ${step.step}: ${duration} [${status}]`);
      }
    }

    if (metrics.errors.length > 0) {
      lines.push("\n--- Errors ---");
      for (const error of metrics.errors.slice(-5)) { // Show last 5 errors
        const location = error.service && error.host
          ? `${error.service}@${error.host}`
          : (error.host || "unknown");
        lines.push(
          `  [${error.type.toUpperCase()}] ${location}: ${error.message}`,
        );
      }
      if (metrics.errors.length > 5) {
        lines.push(`  ... and ${metrics.errors.length - 5} more errors`);
      }
    }

    lines.push("================================================\n");

    return lines.join("\n");
  }

  /**
   * Calculate deployment success rate over recent deployments
   */
  getSuccessRate(limit: number = 10): number {
    const recentMetrics = Array.from(this.metrics.values())
      .filter((m) => m.endTime) // Only completed deployments
      .sort((a, b) => (b.endTime?.getTime() || 0) - (a.endTime?.getTime() || 0))
      .slice(0, limit);

    if (recentMetrics.length === 0) return 0;

    const successCount =
      recentMetrics.filter((m) =>
        m.failedDeployments === 0 && m.errors.length === 0
      ).length;

    return (successCount / recentMetrics.length) * 100;
  }

  /**
   * Get average deployment duration
   */
  getAverageDeploymentDuration(limit: number = 10): number {
    const recentMetrics = Array.from(this.metrics.values())
      .filter((m) => m.totalDurationMs && m.totalDurationMs > 0)
      .sort((a, b) => (b.endTime?.getTime() || 0) - (a.endTime?.getTime() || 0))
      .slice(0, limit);

    if (recentMetrics.length === 0) return 0;

    const totalDuration = recentMetrics.reduce(
      (sum, m) => sum + (m.totalDurationMs || 0),
      0,
    );
    return totalDuration / recentMetrics.length;
  }

  /**
   * Clean up old metrics (keep last N deployments)
   */
  cleanup(keepLast: number = 50): void {
    const allMetrics = Array.from(this.metrics.entries())
      .sort(([, a], [, b]) =>
        (b.timestamp?.getTime() || 0) - (a.timestamp?.getTime() || 0)
      );

    if (allMetrics.length <= keepLast) return;

    const toRemove = allMetrics.slice(keepLast);
    for (const [deploymentId] of toRemove) {
      this.metrics.delete(deploymentId);
    }

    log.debug(
      `Cleaned up ${toRemove.length} old deployment metrics`,
      "metrics",
    );
  }
}

/**
 * Global metrics service instance
 */
export const deploymentMetrics = new DeploymentMetricsService();
