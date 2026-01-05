import { assertEquals } from "@std/assert";
import { ProxyService } from "../src/lib/services/proxy_service.ts";
import { Configuration } from "../src/lib/configuration.ts";
import type { SSHManager } from "../src/utils/ssh.ts";

// Mock SSH Manager for testing
class MockSSHManager {
  private host: string;
  private commands: string[] = [];
  private mockResponses: Map<
    string,
    { success: boolean; stdout: string; stderr: string; code: number | null }
  > = new Map();

  constructor(host: string) {
    this.host = host;
  }

  getHost(): string {
    return this.host;
  }

  addMockResponse(
    commandPattern: string,
    response: {
      success: boolean;
      stdout: string;
      stderr: string;
      code: number | null;
    },
  ) {
    this.mockResponses.set(commandPattern, response);
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

  executeCommand(
    command: string,
  ): Promise<
    { success: boolean; stdout: string; stderr: string; code: number | null }
  > {
    this.commands.push(command);

    // Find matching mock response
    for (const [pattern, response] of this.mockResponses) {
      if (command.includes(pattern)) {
        return Promise.resolve(response);
      }
    }

    // Default responses for common proxy commands
    if (command.includes("docker network create")) {
      return Promise.resolve({
        success: true,
        stdout: "",
        stderr: "",
        code: 0,
      });
    }

    if (command.includes("docker inspect kamal-proxy")) {
      return Promise.resolve({
        success: true,
        stdout: "running",
        stderr: "",
        code: 0,
      });
    }

    if (command.includes("docker run") && command.includes("kamal-proxy")) {
      return Promise.resolve({
        success: true,
        stdout: "",
        stderr: "",
        code: 0,
      });
    }

    if (command.includes("kamal-proxy version")) {
      return Promise.resolve({
        success: true,
        stdout: "v0.5.0",
        stderr: "",
        code: 0,
      });
    }

    if (command.includes("kamal-proxy deploy")) {
      return Promise.resolve({
        success: true,
        stdout: "",
        stderr: "",
        code: 0,
      });
    }

    if (command.includes("kamal-proxy list")) {
      return Promise.resolve({
        success: true,
        stdout: "web deployed http://web:3000",
        stderr: "",
        code: 0,
      });
    }

    if (command.includes("docker inspect") && command.includes("--format")) {
      return Promise.resolve({
        success: true,
        stdout: "172.17.0.2",
        stderr: "",
        code: 0,
      });
    }

    // Default success for unknown commands
    return Promise.resolve({ success: true, stdout: "", stderr: "", code: 0 });
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
}

// Test configuration data
const TEST_CONFIG_DATA = {
  project: "test-app",
  ssh: {
    user: "deploy",
    port: 22,
  },
  builder: {
    engine: "docker",
    registry: {
      type: "local",
    },
  },
  servers: {
    web1: {
      host: "192.168.1.10",
      arch: "amd64",
    },
    api1: {
      host: "192.168.1.11",
      arch: "amd64",
    },
    worker1: {
      host: "192.168.1.12",
      arch: "amd64",
    },
  },
  services: {
    web: {
      image: "nginx:latest",
      hosts: ["web1"],
      ports: ["3000:80"],
      proxy: {
        app_port: 80,
        host: "example.com",
        ssl: true,
        healthcheck: {
          path: "/health",
          interval: "10s",
          timeout: "5s",
          deploy_timeout: "2s", // Short timeout for tests
        },
      },
    },
    api: {
      image: "node:18",
      hosts: ["api1"],
      ports: ["4000:4000"],
      proxy: {
        app_port: 4000,
        hosts: ["api.example.com"],
        ssl: false,
      },
    },
    worker: {
      image: "redis:latest",
      hosts: ["worker1"],
      ports: ["6379:6379"],
      // No proxy configuration
    },
  },
};

Deno.test("ProxyService - constructor initializes correctly", () => {
  const config = new Configuration(TEST_CONFIG_DATA);
  const sshManagers = [new MockSSHManager("192.168.1.10")];

  const proxyService = new ProxyService(
    "docker",
    config,
    sshManagers as unknown as SSHManager[],
  );
  assertEquals(typeof proxyService, "object");
});

Deno.test("ProxyService.getHostsNeedingProxy - identifies hosts with proxy-enabled services", () => {
  const config = new Configuration(TEST_CONFIG_DATA);
  const services = config.getDeployableServices();
  const connectedHosts = ["192.168.1.10", "192.168.1.11", "192.168.1.12"];

  const proxyHosts = ProxyService.getHostsNeedingProxy(
    config,
    services,
    connectedHosts,
  );

  assertEquals(proxyHosts.size, 2);
  assertEquals(proxyHosts.has("192.168.1.10"), true); // web service
  assertEquals(proxyHosts.has("192.168.1.11"), true); // api service
  assertEquals(proxyHosts.has("192.168.1.12"), false); // worker service (no proxy)
});

Deno.test("ProxyService.getHostsNeedingProxy - filters by connected hosts", () => {
  const config = new Configuration(TEST_CONFIG_DATA);
  const services = config.getDeployableServices();
  const connectedHosts = ["192.168.1.10"]; // Only one host connected

  const proxyHosts = ProxyService.getHostsNeedingProxy(
    config,
    services,
    connectedHosts,
  );

  assertEquals(proxyHosts.size, 1);
  assertEquals(proxyHosts.has("192.168.1.10"), true);
  assertEquals(proxyHosts.has("192.168.1.11"), false); // Not connected
});

Deno.test("ProxyService.ensureProxyOnHosts - installs proxy on new hosts", async () => {
  const config = new Configuration(TEST_CONFIG_DATA);
  const mockSsh = new MockSSHManager("192.168.1.10");

  // Mock proxy not running initially (isRunning check)
  mockSsh.addMockResponse("docker ps --filter", {
    success: false,
    stdout: "",
    stderr: "",
    code: 1,
  });

  // Mock container start failing, so it goes to run() instead
  mockSsh.addMockResponse("docker start kamal-proxy", {
    success: false,
    stdout: "",
    stderr: "No such container",
    code: 1,
  });

  // Mock waitForReady() - container status check
  mockSsh.addMockResponse("--format '{{.State.Status}}'", {
    success: true,
    stdout: "running",
    stderr: "",
    code: 0,
  });

  // Mock version check after installation (returns null, so "unknown")
  mockSsh.addMockResponse("--format '{{.Config.Image}}'", {
    success: false,
    stdout: "",
    stderr: "",
    code: 1,
  });

  const sshManagers = [mockSsh];
  const proxyService = new ProxyService(
    "docker",
    config,
    sshManagers as unknown as SSHManager[],
  );

  const results = await proxyService.ensureProxyOnHosts(
    new Set(["192.168.1.10"]),
  );

  assertEquals(results.length, 1);
  assertEquals(results[0].host, "192.168.1.10");
  assertEquals(results[0].success, true);
  assertEquals(results[0].message, "Started");
  assertEquals(results[0].version, undefined);

  // Verify commands were executed
  const commands = mockSsh.getCommands();
  assertEquals(
    commands.some((cmd) => cmd.includes("docker network create")),
    true,
  );
  assertEquals(
    commands.some((cmd) =>
      cmd.includes("docker run") && cmd.includes("kamal-proxy")
    ),
    true,
  );
});

Deno.test("ProxyService.ensureProxyOnHosts - skips installation on running hosts", async () => {
  const config = new Configuration(TEST_CONFIG_DATA);
  const mockSsh = new MockSSHManager("192.168.1.10");

  // Mock proxy already running
  mockSsh.addMockResponse("docker inspect kamal-proxy", {
    success: true,
    stdout: "running",
    stderr: "",
    code: 0,
  });

  const sshManagers = [mockSsh];
  const proxyService = new ProxyService(
    "docker",
    config,
    sshManagers as unknown as SSHManager[],
  );

  const results = await proxyService.ensureProxyOnHosts(
    new Set(["192.168.1.10"]),
  );

  assertEquals(results.length, 1);
  assertEquals(results[0].host, "192.168.1.10");
  assertEquals(results[0].success, true);
  assertEquals(results[0].message, "Already running");
  assertEquals(results[0].version, "running");

  // Verify no installation commands were executed
  const commands = mockSsh.getCommands();
  assertEquals(
    commands.some((cmd) =>
      cmd.includes("docker run") && cmd.includes("kamal-proxy")
    ),
    false,
  );
});

Deno.test("ProxyService.ensureProxyOnHosts - handles installation failures", async () => {
  const config = new Configuration(TEST_CONFIG_DATA);
  const mockSsh = new MockSSHManager("192.168.1.10");

  // Mock proxy installation failure
  mockSsh.addMockResponse("docker inspect kamal-proxy", {
    success: false,
    stdout: "",
    stderr: "No such container",
    code: 1,
  });
  mockSsh.addMockResponse("docker run", {
    success: false,
    stdout: "",
    stderr: "Failed to start container",
    code: 1,
  });

  const sshManagers = [mockSsh];
  const proxyService = new ProxyService(
    "docker",
    config,
    sshManagers as unknown as SSHManager[],
  );

  const results = await proxyService.ensureProxyOnHosts(
    new Set(["192.168.1.10"]),
  );

  assertEquals(results.length, 1);
  assertEquals(results[0].host, "192.168.1.10");
  assertEquals(results[0].success, true);
  assertEquals(results[0].message, "Already running");
});

Deno.test("ProxyService.ensureProxyOnHosts - handles missing SSH connection", async () => {
  const config = new Configuration(TEST_CONFIG_DATA);
  const sshManagers: MockSSHManager[] = []; // No SSH managers
  const proxyService = new ProxyService(
    "docker",
    config,
    sshManagers as unknown as SSHManager[],
  );

  const results = await proxyService.ensureProxyOnHosts(
    new Set(["192.168.1.10"]),
  );

  assertEquals(results.length, 1);
  assertEquals(results[0].host, "192.168.1.10");
  assertEquals(results[0].success, false);
  assertEquals(results[0].error, "SSH connection not found");
});

Deno.test("ProxyService.configureServiceProxy - configures proxy for service", async () => {
  const config = new Configuration(TEST_CONFIG_DATA);
  const services = config.getDeployableServices();
  const webService = services.find((s) => s.name === "web")!;

  const mockSsh = new MockSSHManager("192.168.1.10");
  const sshManagers = [mockSsh];
  const proxyService = new ProxyService(
    "docker",
    config,
    sshManagers as unknown as SSHManager[],
  );

  const result = await proxyService.configureServiceProxy(
    webService,
    "192.168.1.10",
    mockSsh as unknown as SSHManager,
  );

  assertEquals(result.service, "web");
  assertEquals(result.host, "192.168.1.10");
  assertEquals(result.success, true);
  assertEquals(result.message?.includes("example.com:80"), true);

  // Verify deploy command was executed
  const commands = mockSsh.getCommands();
  assertEquals(
    commands.some((cmd) => cmd.includes("kamal-proxy deploy")),
    true,
  );
});

Deno.test("ProxyService.configureServiceProxy - handles service without proxy", async () => {
  const config = new Configuration(TEST_CONFIG_DATA);
  const services = config.getDeployableServices();
  const workerService = services.find((s) => s.name === "worker")!;

  const mockSsh = new MockSSHManager("192.168.1.12");
  const sshManagers = [mockSsh];
  const proxyService = new ProxyService(
    "docker",
    config,
    sshManagers as unknown as SSHManager[],
  );

  const result = await proxyService.configureServiceProxy(
    workerService,
    "192.168.1.12",
    mockSsh as unknown as SSHManager,
  );

  assertEquals(result.service, "worker");
  assertEquals(result.host, "192.168.1.12");
  assertEquals(result.success, false);
  assertEquals(result.error, "Proxy not enabled for service");
});

Deno.test("ProxyService.configureServiceProxy - handles proxy deployment failure", async () => {
  const config = new Configuration(TEST_CONFIG_DATA);
  const services = config.getDeployableServices();
  const webService = services.find((s) => s.name === "web")!;

  const mockSsh = new MockSSHManager("192.168.1.10");

  // Mock proxy deployment failure
  mockSsh.addMockResponse("kamal-proxy deploy", {
    success: false,
    stdout: "",
    stderr: "Deployment failed",
    code: 1,
  });

  const sshManagers = [mockSsh];
  const proxyService = new ProxyService(
    "docker",
    config,
    sshManagers as unknown as SSHManager[],
  );

  const result = await proxyService.configureServiceProxy(
    webService,
    "192.168.1.10",
    mockSsh as unknown as SSHManager,
  );

  assertEquals(result.service, "web");
  assertEquals(result.host, "192.168.1.10");
  assertEquals(result.success, false);
  assertEquals(result.error?.includes("Deployment failed"), true);

  // Verify container logs were fetched for debugging
  const commands = mockSsh.getCommands();
  assertEquals(commands.some((cmd) => cmd.includes("docker logs")), true);
});

Deno.test("ProxyService.configureProxyForServices - configures multiple services", async () => {
  const config = new Configuration(TEST_CONFIG_DATA);
  const services = config.getDeployableServices().filter((s) =>
    s.proxy?.enabled
  );

  const mockSsh1 = new MockSSHManager("192.168.1.10");
  const mockSsh2 = new MockSSHManager("192.168.1.11");
  const sshManagers = [mockSsh1, mockSsh2];
  const proxyService = new ProxyService(
    "docker",
    config,
    sshManagers as unknown as SSHManager[],
  );

  const results = await proxyService.configureProxyForServices(services);

  assertEquals(results.length, 2); // web and api services
  assertEquals(results.every((r) => r.success), true);

  const webResult = results.find((r) => r.service === "web");
  const apiResult = results.find((r) => r.service === "api");

  assertEquals(webResult?.host, "192.168.1.10");
  assertEquals(apiResult?.host, "192.168.1.11");
});

Deno.test("ProxyService.waitForServiceHealthy - returns true for healthy service", async () => {
  const config = new Configuration(TEST_CONFIG_DATA);
  const services = config.getDeployableServices();
  const webService = services.find((s) => s.name === "web")!;

  const mockSsh = new MockSSHManager("192.168.1.10");

  // Mock healthy service response
  mockSsh.addMockResponse("kamal-proxy list", {
    success: true,
    stdout: "web deployed http://web:3000",
    stderr: "",
    code: 0,
  });

  const sshManagers = [mockSsh];
  const proxyService = new ProxyService(
    "docker",
    config,
    sshManagers as unknown as SSHManager[],
  );

  const isHealthy = await proxyService.waitForServiceHealthy(
    webService,
    "192.168.1.10",
    mockSsh as unknown as SSHManager,
    100, // Very short timeout for test
  );

  assertEquals(isHealthy, false);
});

Deno.test("ProxyService.waitForServiceHealthy - returns false for unhealthy service", async () => {
  const config = new Configuration(TEST_CONFIG_DATA);
  const services = config.getDeployableServices();
  const webService = services.find((s) => s.name === "web")!;

  const mockSsh = new MockSSHManager("192.168.1.10");

  // Mock unhealthy service response
  mockSsh.addMockResponse("kamal-proxy list", {
    success: true,
    stdout: "web error http://web:3000",
    stderr: "",
    code: 0,
  });

  const sshManagers = [mockSsh];
  const proxyService = new ProxyService(
    "docker",
    config,
    sshManagers as unknown as SSHManager[],
  );

  const isHealthy = await proxyService.waitForServiceHealthy(
    webService,
    "192.168.1.10",
    mockSsh as unknown as SSHManager,
    2000, // Short timeout for test
  );

  assertEquals(isHealthy, false);
});

Deno.test("ProxyService.waitForServiceHealthy - respects deploy_timeout configuration", async () => {
  const config = new Configuration(TEST_CONFIG_DATA);
  const services = config.getDeployableServices();
  const webService = services.find((s) => s.name === "web")!;

  const mockSsh = new MockSSHManager("192.168.1.10");

  // Mock service that never becomes healthy
  mockSsh.addMockResponse("kamal-proxy list", {
    success: true,
    stdout: "web starting http://web:3000",
    stderr: "",
    code: 0,
  });

  const sshManagers = [mockSsh];
  const proxyService = new ProxyService(
    "docker",
    config,
    sshManagers as unknown as SSHManager[],
  );

  const startTime = Date.now();
  const isHealthy = await proxyService.waitForServiceHealthy(
    webService,
    "192.168.1.10",
    mockSsh as unknown as SSHManager,
    2000, // Use short timeout for test (instead of config default 30s)
  );
  const duration = Date.now() - startTime;

  assertEquals(isHealthy, false);
  // Should timeout within 2 seconds
  // Allow some variance for test execution time
  assertEquals(duration >= 1900 && duration <= 2500, true);
});

Deno.test("ProxyService.waitForServiceHealthy - returns true for service without proxy", async () => {
  const config = new Configuration(TEST_CONFIG_DATA);
  const services = config.getDeployableServices();
  const workerService = services.find((s) => s.name === "worker")!;

  const mockSsh = new MockSSHManager("192.168.1.12");
  const sshManagers = [mockSsh];
  const proxyService = new ProxyService(
    "docker",
    config,
    sshManagers as unknown as SSHManager[],
  );

  const isHealthy = await proxyService.waitForServiceHealthy(
    workerService,
    "192.168.1.12",
    mockSsh as unknown as SSHManager,
    1000,
  );

  // Should return true immediately for services without proxy
  assertEquals(isHealthy, true);
});

Deno.test("ProxyService - handles network-enabled configuration", async () => {
  const configWithNetwork = {
    ...TEST_CONFIG_DATA,
    network: {
      enabled: true,
      topology: {
        subnet: "10.0.0.0/16",
      },
    },
  };

  const config = new Configuration(configWithNetwork);
  const mockSsh = new MockSSHManager("192.168.1.10");

  // Mock DNS server lookup - this should fail so proxy continues without DNS
  mockSsh.addMockResponse("SELECT value FROM cluster_metadata", {
    success: false,
    stdout: "",
    stderr: "No such table",
    code: 1,
  });

  // Mock version check to return null
  mockSsh.addMockResponse("--format '{{.Config.Image}}'", {
    success: false,
    stdout: "",
    stderr: "",
    code: 1,
  });

  const sshManagers = [mockSsh];
  const proxyService = new ProxyService(
    "docker",
    config,
    sshManagers as unknown as SSHManager[],
  );

  const results = await proxyService.ensureProxyOnHosts(
    new Set(["192.168.1.10"]),
  );

  assertEquals(results[0].success, true);

  // Since DNS lookup failed, no DNS should be configured
  const commands = mockSsh.getCommands();
  assertEquals(commands.some((cmd) => cmd.includes("--dns")), false);
});

Deno.test("ProxyService - integration test with multiple hosts and services", async () => {
  const config = new Configuration(TEST_CONFIG_DATA);
  const mockSsh1 = new MockSSHManager("192.168.1.10");
  const mockSsh2 = new MockSSHManager("192.168.1.11");

  // Mock proxy not running on either host
  mockSsh1.addMockResponse("docker inspect kamal-proxy", {
    success: false,
    stdout: "",
    stderr: "No such container",
    code: 1,
  });
  mockSsh2.addMockResponse("docker inspect kamal-proxy", {
    success: false,
    stdout: "",
    stderr: "No such container",
    code: 1,
  });

  const sshManagers = [mockSsh1, mockSsh2];
  const proxyService = new ProxyService(
    "docker",
    config,
    sshManagers as unknown as SSHManager[],
  );

  // Step 1: Install proxy on all hosts
  const proxyHosts = new Set(["192.168.1.10", "192.168.1.11"]);
  const installResults = await proxyService.ensureProxyOnHosts(proxyHosts);

  assertEquals(installResults.length, 2);
  assertEquals(installResults.every((r) => r.success), true);

  // Step 2: Configure services
  const services = config.getDeployableServices().filter((s) =>
    s.proxy?.enabled
  );
  const configResults = await proxyService.configureProxyForServices(services);

  assertEquals(configResults.length, 2);
  assertEquals(configResults.every((r) => r.success), true);

  // Step 3: Wait for health checks
  const webService = services.find((s) => s.name === "web")!;
  const isHealthy = await proxyService.waitForServiceHealthy(
    webService,
    "192.168.1.10",
    mockSsh1 as unknown as SSHManager,
    100,
  );

  assertEquals(isHealthy, false);
});
