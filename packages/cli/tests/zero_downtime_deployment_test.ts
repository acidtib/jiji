import { assertEquals, assertExists } from "@std/assert";
import { DeploymentOrchestrator } from "../src/lib/services/deployment_orchestrator.ts";
import { Configuration } from "../src/lib/configuration.ts";
import type { SSHManager } from "../src/utils/ssh.ts";

// Mock SSH Manager for integration testing
class IntegrationMockSSHManager {
  private host: string;
  private commands: string[] = [];
  private containerStates: Map<string, "running" | "stopped" | "missing"> =
    new Map();
  private proxyServices: Map<string, "deployed" | "starting" | "error"> =
    new Map();
  private failHealthChecks = false;
  private deploymentDelay = 0;

  constructor(host: string) {
    this.host = host;
  }

  getHost(): string {
    return this.host;
  }

  // Test setup methods
  setContainerState(
    containerName: string,
    state: "running" | "stopped" | "missing",
  ) {
    this.containerStates.set(containerName, state);
  }

  setProxyServiceState(
    serviceName: string,
    state: "deployed" | "starting" | "error",
  ) {
    this.proxyServices.set(serviceName, state);
  }

  setFailHealthChecks(fail: boolean) {
    this.failHealthChecks = fail;
  }

  setDeploymentDelay(delayMs: number) {
    this.deploymentDelay = delayMs;
  }

  addMockResponse(
    _commandPattern: string,
    _response: {
      success: boolean;
      stdout: string;
      stderr: string;
      code: number | null;
    },
  ) {
    // For integration tests, we'll handle this by modifying the executeCommand method
    // This is a simplified approach - in practice you might want to store these responses
  }

  // Additional methods to match SSHManager interface
  getConfig() {
    return { host: this.host };
  }

  isConnected(): boolean {
    return true;
  }

  async connect(): Promise<void> {
    // Mock connect
  }

  async disconnect(): Promise<void> {
    // Mock disconnect
  }

  async executeCommand(
    command: string,
  ): Promise<
    { success: boolean; stdout: string; stderr: string; code: number | null }
  > {
    this.commands.push(command);

    // Add deployment delay simulation
    if (this.deploymentDelay > 0) {
      await new Promise((resolve) => setTimeout(resolve, this.deploymentDelay));
    }

    // Handle proxy network creation
    if (command.includes("docker network create kamal-proxy")) {
      return { success: true, stdout: "", stderr: "", code: 0 };
    }

    // Handle proxy container checks
    if (command.includes("docker inspect kamal-proxy")) {
      if (command.includes("HostConfig.Binds")) {
        return {
          success: true,
          stdout: '["$HOME/.jiji/certs:/jiji-certs:ro"]',
          stderr: "",
          code: 0,
        };
      }
      return { success: true, stdout: "running", stderr: "", code: 0 };
    }

    // Handle proxy boot
    if (command.includes("docker run") && command.includes("kamal-proxy")) {
      return { success: true, stdout: "", stderr: "", code: 0 };
    }

    // Handle proxy version
    if (command.includes("kamal-proxy version")) {
      return { success: true, stdout: "v0.5.0", stderr: "", code: 0 };
    }

    // Handle container existence checks (docker ps -a --filter)
    if (
      command.includes("docker ps -a --filter") && command.includes("name=")
    ) {
      const containerName = this.extractContainerNameFromFilter(command);
      const state = this.containerStates.get(containerName) || "missing";

      if (state === "missing") {
        return { success: false, stdout: "", stderr: "", code: 1 };
      }
      return { success: true, stdout: containerName, stderr: "", code: 0 };
    }

    // Handle container existence checks (docker inspect)
    if (
      command.includes("docker inspect") && !command.includes("kamal-proxy")
    ) {
      const containerName = this.extractContainerNameFromInspect(command);
      const state = this.containerStates.get(containerName) || "missing";

      if (state === "missing") {
        return {
          success: false,
          stdout: "",
          stderr: `No such container: ${containerName}`,
          code: 1,
        };
      }
      return { success: true, stdout: state, stderr: "", code: 0 };
    }

    // Handle container IP inspection
    if (command.includes("--format '{{.NetworkSettings.IPAddress}}'")) {
      return { success: true, stdout: "172.17.0.2", stderr: "", code: 0 };
    }

    // Handle container renaming
    if (command.includes("docker rename")) {
      const [oldName, newName] = this.extractRenameParams(command);
      const oldState = this.containerStates.get(oldName);
      if (oldState) {
        this.containerStates.delete(oldName);
        this.containerStates.set(newName, oldState);
      }
      return { success: true, stdout: "", stderr: "", code: 0 };
    }

    // Handle container removal
    if (command.includes("docker rm")) {
      const containerName = this.extractContainerNameFromRemove(command);
      this.containerStates.delete(containerName);
      return { success: true, stdout: "", stderr: "", code: 0 };
    }

    // Handle container run (deployment)
    if (command.includes("docker run")) {
      const containerName = this.extractContainerNameFromRun(command);
      this.containerStates.set(containerName, "running");
      return { success: true, stdout: "", stderr: "", code: 0 };
    }

    // Handle proxy deployment
    if (command.includes("kamal-proxy deploy")) {
      const serviceName = this.extractServiceNameFromProxyDeploy(command);
      this.proxyServices.set(serviceName, "deployed");
      return { success: true, stdout: "", stderr: "", code: 0 };
    }

    // Handle proxy service listing for health checks
    if (command.includes("kamal-proxy list")) {
      const services: string[] = [];
      services.push("Service  Host  Path  Target  State  TLS");
      services.push("-------  ----  ----  ------  -----  ---");

      for (const [serviceName, state] of this.proxyServices) {
        const healthState = this.failHealthChecks ? "error" : state;
        const host = serviceName === "web" ? "example.com" : "api.example.com";
        const tls = serviceName === "web" ? "yes" : "no";
        services.push(
          `${serviceName}  ${host}  /  http://${serviceName}:3000  ${healthState}  ${tls}`,
        );
      }
      return {
        success: true,
        stdout: services.join("\n"),
        stderr: "",
        code: 0,
      };
    }

    // Handle container logs
    if (command.includes("docker logs")) {
      return {
        success: true,
        stdout: "Mock container logs for debugging",
        stderr: "",
        code: 0,
      };
    }

    // Handle image pulling
    if (command.includes("docker pull")) {
      return { success: true, stdout: "Pull complete", stderr: "", code: 0 };
    }

    // Handle directory creation
    if (command.includes("mkdir -p")) {
      return { success: true, stdout: "", stderr: "", code: 0 };
    }

    // Handle file uploads (rsync, scp, etc.)
    if (
      command.includes("rsync") || command.includes("scp") ||
      command.includes("tar")
    ) {
      return { success: true, stdout: "", stderr: "", code: 0 };
    }

    // Default success for unknown commands
    return { success: true, stdout: "", stderr: "", code: 0 };
  }

  getCommands(): string[] {
    return [...this.commands];
  }

  clearCommands(): void {
    this.commands = [];
  }

  async dispose(): Promise<void> {
    // Mock cleanup
  }

  // Helper methods for parsing commands
  private extractContainerNameFromInspect(command: string): string {
    const match = command.match(/docker inspect ([^\s]+)/);
    return match ? match[1] : "unknown";
  }

  private extractRenameParams(command: string): [string, string] {
    const match = command.match(/docker rename ([^\s]+) ([^\s]+)/);
    return match ? [match[1], match[2]] : ["", ""];
  }

  private extractContainerNameFromRemove(command: string): string {
    const match = command.match(/docker rm[^\s]*\s+([^\s]+)/);
    return match ? match[1] : "unknown";
  }

  private extractContainerNameFromRun(command: string): string {
    const match = command.match(/--name ([^\s]+)/);
    return match ? match[1] : "unknown";
  }

  private extractServiceNameFromProxyDeploy(command: string): string {
    const match = command.match(/kamal-proxy deploy ([^\s]+)/);
    return match ? match[1] : "unknown";
  }

  private extractContainerNameFromFilter(command: string): string {
    const match = command.match(/name=\^([^$]+)\$/);
    return match ? match[1] : "unknown";
  }
}

// Test configuration for zero-downtime deployment
const ZERO_DOWNTIME_CONFIG = {
  project: "zero-downtime-test",
  ssh: {
    user: "root",
  },
  builder: {
    engine: "docker",
    registry: {
      type: "local",
    },
  },
  servers: {
    "web-server-1": {
      host: "web-server-1",
      arch: "amd64",
    },
    "web-server-2": {
      host: "web-server-2",
      arch: "amd64",
    },
    "api-server-1": {
      host: "api-server-1",
      arch: "amd64",
    },
  },
  services: {
    web: {
      image: "nginx:latest",
      hosts: ["web-server-1", "web-server-2"],
      ports: ["3000:80"],
      proxy: {
        enabled: true,
        host: "example.com",
        ssl: true,
        healthcheck: {
          path: "/health",
          interval: "10s",
          timeout: "5s",
          deploy_timeout: "2s", // Short timeout for tests
        },
      },
      retain: 3,
    },
    api: {
      image: "node:18",
      hosts: ["api-server-1"],
      ports: ["4000:4000"],
      proxy: {
        enabled: true,
        hosts: ["api.example.com"],
        ssl: false,
        healthcheck: {
          path: "/api/health",
          interval: "5s",
          timeout: "3s",
          deploy_timeout: "2s", // Short timeout for tests
        },
      },
      retain: 2,
    },
  },
};

Deno.test("Zero-downtime deployment - fresh deployment (no existing containers)", async () => {
  const config = new Configuration(ZERO_DOWNTIME_CONFIG);
  const services = config.getDeployableServices();

  // Create mock SSH managers for each host
  const webMock1 = new IntegrationMockSSHManager("web-server-1");
  const webMock2 = new IntegrationMockSSHManager("web-server-2");
  const apiMock = new IntegrationMockSSHManager("api-server-1");

  // No existing containers (fresh deployment)
  const sshManagers = [webMock1, webMock2, apiMock];
  const targetHosts = ["web-server-1", "web-server-2", "api-server-1"];

  const orchestrator = new DeploymentOrchestrator(
    config,
    sshManagers as unknown as SSHManager[],
  );

  const result = await orchestrator.orchestrateDeployment(
    services,
    targetHosts,
    { version: "v1.0.0" },
  );

  // Verify overall success
  assertEquals(result.success, true);
  assertEquals(result.errors.length, 0);

  // Verify proxy installation
  assertEquals(result.proxyInstallResults.length, 3); // All hosts need proxy
  assertEquals(
    result.proxyInstallResults.every((r) => r.success),
    true,
  );

  // Verify service deployments
  assertEquals(result.deploymentResults.length, 3); // web x2, api x1
  assertEquals(
    result.deploymentResults.every((r) => r.success),
    true,
  );

  // Verify proxy configuration
  assertEquals(result.proxyConfigResults.length, 3); // web x2, api x1
  assertEquals(
    result.proxyConfigResults.every((r) => r.success),
    true,
  );

  // Verify no old containers to cleanup (fresh deployment)
  assertEquals(
    result.deploymentResults.every((r) => !r.oldContainerName),
    true,
  );
});

Deno.test("Zero-downtime deployment - update existing containers", async () => {
  const config = new Configuration(ZERO_DOWNTIME_CONFIG);
  const services = config.getDeployableServices();

  const webMock1 = new IntegrationMockSSHManager("web-server-1");
  const webMock2 = new IntegrationMockSSHManager("web-server-2");
  const apiMock = new IntegrationMockSSHManager("api-server-1");

  // Set existing containers as running
  webMock1.setContainerState("zero-downtime-test-web", "running");
  webMock2.setContainerState("zero-downtime-test-web", "running");
  apiMock.setContainerState("zero-downtime-test-api", "running");

  const sshManagers = [webMock1, webMock2, apiMock];
  const targetHosts = ["web-server-1", "web-server-2", "api-server-1"];

  const orchestrator = new DeploymentOrchestrator(
    config,
    sshManagers as unknown as SSHManager[],
  );

  const result = await orchestrator.orchestrateDeployment(
    services,
    targetHosts,
    { version: "v1.1.0" },
  );

  // Verify overall success
  assertEquals(result.success, true);
  assertEquals(result.errors.length, 0);

  // Verify deployments had old containers to rename
  assertEquals(result.deploymentResults.length, 3);
  assertEquals(
    result.deploymentResults.every((r) => r.success),
    true,
  );

  // Check that rename operations occurred
  const webCommands1 = webMock1.getCommands();
  const webCommands2 = webMock2.getCommands();
  const apiCommands = apiMock.getCommands();

  assertEquals(
    webCommands1.some((cmd) => cmd.includes("docker rename")),
    true,
  );
  assertEquals(
    webCommands2.some((cmd) => cmd.includes("docker rename")),
    true,
  );
  assertEquals(
    apiCommands.some((cmd) => cmd.includes("docker rename")),
    true,
  );

  // Check that old containers were eventually cleaned up
  assertEquals(
    webCommands1.some((cmd) =>
      cmd.includes("docker rm") && cmd.includes("_old_")
    ),
    true,
  );
  assertEquals(
    webCommands2.some((cmd) =>
      cmd.includes("docker rm") && cmd.includes("_old_")
    ),
    true,
  );
  assertEquals(
    apiCommands.some((cmd) =>
      cmd.includes("docker rm") && cmd.includes("_old_")
    ),
    true,
  );
});

Deno.test("Zero-downtime deployment - rollback on health check failure", async () => {
  const config = new Configuration(ZERO_DOWNTIME_CONFIG);
  const services = config.getDeployableServices();

  const webMock1 = new IntegrationMockSSHManager("web-server-1");
  const apiMock = new IntegrationMockSSHManager("api-server-1");

  // Set existing containers as running
  webMock1.setContainerState("zero-downtime-test-web", "running");
  apiMock.setContainerState("zero-downtime-test-api", "running");

  // Make health checks fail for web service
  webMock1.setFailHealthChecks(true);

  const sshManagers = [webMock1, apiMock];
  const targetHosts = ["web-server-1", "api-server-1"];

  const orchestrator = new DeploymentOrchestrator(
    config,
    sshManagers as unknown as SSHManager[],
  );

  const result = await orchestrator.orchestrateDeployment(
    services,
    targetHosts,
    { version: "v1.2.0" },
  );

  // Overall should fail due to rollback
  assertEquals(result.success, false);
  assertEquals(result.errors.length > 0, true);

  // Should contain rollback-related error
  const hasRollbackError = result.errors.some((error) =>
    error.toLowerCase().includes("health check") ||
    error.toLowerCase().includes("rollback")
  );
  assertEquals(hasRollbackError, true);

  // Check that rollback operations occurred for web service
  const webCommands = webMock1.getCommands();

  // Should have renamed old container, then renamed it back
  const renameCommands = webCommands.filter((cmd) =>
    cmd.includes("docker rename")
  );
  assertEquals(renameCommands.length >= 2, true); // At least rename to _old_ and back

  // Should have removed the failed new container
  assertEquals(
    webCommands.some((cmd) =>
      cmd.includes("docker rm") && !cmd.includes("_old_")
    ),
    true,
  );

  // API service should still succeed (no health check failure)
  const apiResults = result.deploymentResults.filter((r) =>
    r.service === "api"
  );
  assertEquals(apiResults.length > 0, true);
  assertEquals(apiResults.every((r) => r.success), true);
});

Deno.test("Zero-downtime deployment - partial proxy installation failure", async () => {
  const config = new Configuration(ZERO_DOWNTIME_CONFIG);
  const services = config.getDeployableServices();

  const webMock1 = new IntegrationMockSSHManager("web-server-1");
  const webMock2 = new IntegrationMockSSHManager("web-server-2");
  const apiMock = new IntegrationMockSSHManager("api-server-1");

  // Make proxy installation fail on one host by overriding executeCommand
  const originalExecuteCommand = webMock2.executeCommand.bind(webMock2);
  webMock2.executeCommand = (command: string) => {
    // Make proxy appear as not running so installation is attempted
    if (
      command.includes("docker ps --filter") &&
      command.includes("kamal-proxy")
    ) {
      return Promise.resolve({
        success: false,
        stdout: "",
        stderr: "",
        code: 1,
      });
    }
    // Make proxy boot fail
    if (command.includes("docker run") && command.includes("kamal-proxy")) {
      return Promise.resolve({
        success: false,
        stdout: "",
        stderr: "Failed to start kamal-proxy",
        code: 1,
      });
    }
    // Make proxy waitForReady check fail
    if (
      command.includes("docker inspect kamal-proxy") &&
      command.includes("--format '{{.State.Status}}'")
    ) {
      return Promise.resolve({
        success: true,
        stdout: "exited",
        stderr: "",
        code: 0,
      });
    }
    return originalExecuteCommand(command);
  };

  const sshManagers = [webMock1, webMock2, apiMock];
  const targetHosts = ["web-server-1", "web-server-2", "api-server-1"];

  const orchestrator = new DeploymentOrchestrator(
    config,
    sshManagers as unknown as SSHManager[],
  );

  const result = await orchestrator.orchestrateDeployment(
    services,
    targetHosts,
    { version: "v1.3.0" },
  );

  // Should fail overall due to proxy installation failure
  assertEquals(result.success, false);

  // Should have proxy installation results with at least one failure
  assertEquals(result.proxyInstallResults.length, 3);
  const failedProxyInstalls = result.proxyInstallResults.filter((r) =>
    !r.success
  );
  assertEquals(failedProxyInstalls.length, 1);
  assertEquals(failedProxyInstalls[0].error?.includes("kamal-proxy"), true);

  // Should have error message about proxy installation failure
  const hasProxyError = result.errors.some((error) =>
    error.toLowerCase().includes("proxy installation failed")
  );
  assertEquals(hasProxyError, true);
});

Deno.test("Zero-downtime deployment - service deployment summary", async () => {
  const config = new Configuration(ZERO_DOWNTIME_CONFIG);
  const services = config.getDeployableServices();

  const webMock1 = new IntegrationMockSSHManager("web-server-1");
  const apiMock = new IntegrationMockSSHManager("api-server-1");

  const sshManagers = [webMock1, apiMock];
  const targetHosts = ["web-server-1", "api-server-1"];

  const orchestrator = new DeploymentOrchestrator(
    config,
    sshManagers as unknown as SSHManager[],
  );

  const result = await orchestrator.orchestrateDeployment(
    services,
    targetHosts,
    { version: "v2.0.0" },
  );

  // Test deployment summary generation
  const summary = orchestrator.getDeploymentSummary(result);

  assertEquals(summary.totalServices, 3); // web@web-server-1 + web@web-server-2 (skipped) + api@api-server-1
  assertEquals(summary.successfulDeployments, 2); // web@web-server-1 + api@api-server-1
  assertEquals(summary.failedDeployments, 1); // web@web-server-2 (unreachable)
  assertEquals(summary.proxyInstallations, 2); // 2 hosts with proxy
  assertEquals(summary.proxyConfigurations, 2); // 2 services with proxy
  assertEquals(summary.hasErrors, true); // Due to unreachable host
  assertEquals(summary.hasWarnings, false);

  // Verify result structure
  assertExists(result.proxyInstallResults);
  assertExists(result.deploymentResults);
  assertExists(result.proxyConfigResults);
  assertExists(result.errors);
  assertExists(result.warnings);
});

Deno.test("Zero-downtime deployment - concurrent deployment timing", async () => {
  const config = new Configuration(ZERO_DOWNTIME_CONFIG);
  const services = config.getDeployableServices();

  const webMock1 = new IntegrationMockSSHManager("web-server-1");
  const webMock2 = new IntegrationMockSSHManager("web-server-2");

  // Add realistic deployment delays
  webMock1.setDeploymentDelay(100); // 100ms
  webMock2.setDeploymentDelay(150); // 150ms

  // Set existing containers for zero-downtime scenario
  webMock1.setContainerState("zero-downtime-test-web", "running");
  webMock2.setContainerState("zero-downtime-test-web", "running");

  const sshManagers = [webMock1, webMock2];
  const targetHosts = ["web-server-1", "web-server-2"];

  const orchestrator = new DeploymentOrchestrator(
    config,
    sshManagers as unknown as SSHManager[],
  );

  const startTime = Date.now();
  const result = await orchestrator.orchestrateDeployment(
    services.filter((s) => s.name === "web"), // Only web service
    targetHosts,
    { version: "v2.1.0" },
  );
  const endTime = Date.now();

  // Verify successful deployment
  assertEquals(result.success, true);

  // Verify timing - should be concurrent, not sequential
  const totalDuration = endTime - startTime;
  // Should be less than 10 seconds (includes proxy setup, health checks, etc.)
  // The key is that it's not taking 2x as long as it would for sequential deployment
  assertEquals(totalDuration < 10000, true);

  // Verify both hosts were deployed to
  assertEquals(result.deploymentResults.length, 2);
  assertEquals(
    result.deploymentResults.every((r) => r.success),
    true,
  );

  // Verify old containers were handled
  assertEquals(
    result.deploymentResults.every((r) => r.oldContainerName),
    true,
  );
});

Deno.test("Zero-downtime deployment - mixed service types (with and without proxy)", async () => {
  const mixedConfig = {
    ...ZERO_DOWNTIME_CONFIG,
    servers: {
      "web-server-1": {
        host: "web-server-1",
        arch: "amd64",
      },
      "worker-server": {
        host: "worker-server",
        arch: "amd64",
      },
    },
    services: {
      web: {
        image: "nginx:latest",
        hosts: ["web-server-1"], // Only include available host
        ports: ["3000:80"],
        proxy: {
          enabled: true,
          host: "example.com",
          ssl: true,
          healthcheck: {
            path: "/health",
            interval: "10s",
            timeout: "5s",
            deploy_timeout: "2s",
          },
        },
        retain: 3,
      },
      worker: {
        image: "redis:latest",
        hosts: ["worker-server"],
        ports: ["6379:6379"],
        // No proxy configuration
        retain: 1,
      },
    },
  };

  const config = new Configuration(mixedConfig);
  const services = config.getDeployableServices();

  const webMock = new IntegrationMockSSHManager("web-server-1");
  const workerMock = new IntegrationMockSSHManager("worker-server");

  const sshManagers = [webMock, workerMock];
  const targetHosts = ["web-server-1", "worker-server"];

  const orchestrator = new DeploymentOrchestrator(
    config,
    sshManagers as unknown as SSHManager[],
  );

  const result = await orchestrator.orchestrateDeployment(
    services,
    targetHosts,
    { version: "v3.0.0" },
  );

  // Verify overall success
  assertEquals(result.success, true);

  // Verify proxy only installed on web server (has proxy-enabled service)
  assertEquals(result.proxyInstallResults.length, 1);
  assertEquals(result.proxyInstallResults[0].host, "web-server-1");

  // Verify all services deployed
  assertEquals(result.deploymentResults.length, 2); // web + worker
  assertEquals(
    result.deploymentResults.every((r) => r.success),
    true,
  );

  // Verify only web service has proxy configuration
  assertEquals(result.proxyConfigResults.length, 1);
  assertEquals(result.proxyConfigResults[0].service, "web");

  // Verify worker service deployment (without proxy)
  const workerResult = result.deploymentResults.find((r) =>
    r.service === "worker"
  );
  assertExists(workerResult);
  assertEquals(workerResult.success, true);
  assertEquals(workerResult.host, "worker-server");
});
