import { assertEquals, assertStringIncludes } from "@std/assert";
import {
  buildDeployCommandArgs,
  buildKamalProxyOptions,
  extractAppPort,
  type KamalProxyDeployOptions,
  ProxyCommands,
} from "../proxy.ts";
import { ProxyConfiguration } from "../../lib/configuration/proxy.ts";
import { MockSSHManager } from "../../../tests/mocks.ts";
import type { SSHManager } from "../ssh.ts";

Deno.test("ProxyCommands - deploy with host only", async () => {
  const mockSSH = new MockSSHManager();
  const proxyCommands = new ProxyCommands(
    "docker",
    mockSSH as unknown as SSHManager,
  );

  const config = new ProxyConfiguration({
    ssl: false,
    host: "example.com",
  });

  await proxyCommands.deploy(
    "test-service",
    "test-container",
    config,
    3000,
    "myproject",
  );

  const lastCommand = mockSSH.getLastCommand();
  assertStringIncludes(lastCommand, "kamal-proxy deploy test-service");
  assertStringIncludes(
    lastCommand,
    "--target=myproject-test-service.jiji:3000",
  );
  assertStringIncludes(lastCommand, "--host=example.com");
  // Should not include SSL or path prefix
  assertEquals(lastCommand.includes("--tls"), false);
  assertEquals(lastCommand.includes("--path-prefix"), false);
});

Deno.test("ProxyCommands - deploy with SSL enabled", async () => {
  const mockSSH = new MockSSHManager();
  const proxyCommands = new ProxyCommands(
    "docker",
    mockSSH as unknown as SSHManager,
  );

  const config = new ProxyConfiguration({
    ssl: true,
    host: "secure.example.com",
  });

  await proxyCommands.deploy(
    "secure-service",
    "secure-container",
    config,
    8080,
    "myproject",
  );

  const lastCommand = mockSSH.getLastCommand();
  assertStringIncludes(lastCommand, "kamal-proxy deploy secure-service");
  assertStringIncludes(
    lastCommand,
    "--target=myproject-secure-service.jiji:8080",
  );
  assertStringIncludes(lastCommand, "--host=secure.example.com");
  assertStringIncludes(lastCommand, "--tls");
});

Deno.test("ProxyCommands - deploy with path prefix", async () => {
  const mockSSH = new MockSSHManager();
  const proxyCommands = new ProxyCommands(
    "docker",
    mockSSH as unknown as SSHManager,
  );

  const config = new ProxyConfiguration({
    ssl: false,
    host: "api.example.com",
    path_prefix: "/v1",
  });

  await proxyCommands.deploy(
    "api-service",
    "api-container",
    config,
    4000,
    "myproject",
  );

  const lastCommand = mockSSH.getLastCommand();
  assertStringIncludes(lastCommand, "kamal-proxy deploy api-service");
  assertStringIncludes(lastCommand, "--target=myproject-api-service.jiji:4000");
  assertStringIncludes(lastCommand, "--host=api.example.com");
  assertStringIncludes(lastCommand, "--path-prefix=/v1");
});

Deno.test("ProxyCommands - deploy with complex path prefix", async () => {
  const mockSSH = new MockSSHManager();
  const proxyCommands = new ProxyCommands(
    "docker",
    mockSSH as unknown as SSHManager,
  );

  const config = new ProxyConfiguration({
    ssl: true,
    host: "app.example.com",
    path_prefix: "/api/v2/graphql",
  });

  await proxyCommands.deploy(
    "graphql-service",
    "graphql-container",
    config,
    5000,
    "myproject",
  );

  const lastCommand = mockSSH.getLastCommand();
  assertStringIncludes(lastCommand, "kamal-proxy deploy graphql-service");
  assertStringIncludes(
    lastCommand,
    "--target=myproject-graphql-service.jiji:5000",
  );
  assertStringIncludes(lastCommand, "--host=app.example.com");
  assertStringIncludes(lastCommand, "--path-prefix=/api/v2/graphql");
  assertStringIncludes(lastCommand, "--tls");
});

Deno.test("ProxyCommands - deploy with all options", async () => {
  const mockSSH = new MockSSHManager();
  const proxyCommands = new ProxyCommands(
    "podman",
    mockSSH as unknown as SSHManager,
  );

  const config = new ProxyConfiguration({
    ssl: true,
    host: "full.example.com",
    path_prefix: "/admin",
    healthcheck: {
      path: "/admin/health",
      interval: "30s",
    },
  });

  await proxyCommands.deploy(
    "admin-service",
    "admin-container",
    config,
    3001,
    "myproject",
  );

  const lastCommand = mockSSH.getLastCommand();
  assertStringIncludes(lastCommand, "podman exec kamal-proxy");
  assertStringIncludes(lastCommand, "kamal-proxy deploy admin-service");
  assertStringIncludes(
    lastCommand,
    "--target=myproject-admin-service.jiji:3001",
  );
  assertStringIncludes(lastCommand, "--host=full.example.com");
  assertStringIncludes(lastCommand, "--path-prefix=/admin");
  assertStringIncludes(lastCommand, "--tls");
  assertStringIncludes(lastCommand, "--health-check-path=/admin/health");
  assertStringIncludes(lastCommand, "--health-check-interval=30s");
});

Deno.test("ProxyCommands - deploy with health check only", async () => {
  const mockSSH = new MockSSHManager();
  const proxyCommands = new ProxyCommands(
    "docker",
    mockSSH as unknown as SSHManager,
  );

  const config = new ProxyConfiguration({
    ssl: false,
    host: "health.example.com",
    healthcheck: {
      path: "/status",
      interval: "10s",
    },
  });

  await proxyCommands.deploy(
    "health-service",
    "health-container",
    config,
    8000,
    "myproject",
  );

  const lastCommand = mockSSH.getLastCommand();
  assertStringIncludes(lastCommand, "kamal-proxy deploy health-service");
  assertStringIncludes(
    lastCommand,
    "--target=myproject-health-service.jiji:8000",
  );
  assertStringIncludes(lastCommand, "--host=health.example.com");
  assertStringIncludes(lastCommand, "--health-check-path=/status");
  assertStringIncludes(lastCommand, "--health-check-interval=10s");
  assertEquals(lastCommand.includes("--tls"), false);
  assertEquals(lastCommand.includes("--path-prefix"), false);
});

Deno.test("ProxyCommands - deploy with partial health check", async () => {
  const mockSSH = new MockSSHManager();
  const proxyCommands = new ProxyCommands(
    "docker",
    mockSSH as unknown as SSHManager,
  );

  const config = new ProxyConfiguration({
    ssl: false,
    host: "partial.example.com",
    healthcheck: {
      path: "/ping",
      // No interval specified
    },
  });

  await proxyCommands.deploy(
    "partial-service",
    "partial-container",
    config,
    9000,
    "myproject",
  );

  const lastCommand = mockSSH.getLastCommand();
  assertStringIncludes(lastCommand, "--health-check-path=/ping");
  assertEquals(lastCommand.includes("--health-check-interval"), false);
});

Deno.test("ProxyCommands - deploy command failure", async () => {
  const mockSSH = new MockSSHManager(false); // Configure to fail
  const proxyCommands = new ProxyCommands(
    "docker",
    mockSSH as unknown as SSHManager,
  );

  const config = new ProxyConfiguration({
    ssl: false,
    host: "fail.example.com",
  });

  let errorThrown = false;
  try {
    await proxyCommands.deploy(
      "fail-service",
      "fail-container",
      config,
      3000,
      "myproject",
    );
  } catch (error) {
    errorThrown = true;
    assertStringIncludes(
      (error as Error).message,
      "Failed to deploy service fail-service to proxy",
    );
  }

  assertEquals(errorThrown, true);
});

Deno.test("extractAppPort - simple port mapping", () => {
  const ports = ["3000:80"];
  assertEquals(extractAppPort(ports), 80);
});

Deno.test("extractAppPort - container port only", () => {
  const ports = ["8000"];
  assertEquals(extractAppPort(ports), 8000);
});

Deno.test("extractAppPort - container port only with protocol", () => {
  const ports = ["9000/tcp"];
  assertEquals(extractAppPort(ports), 9000);
});

Deno.test("extractAppPort - with host IP", () => {
  const ports = ["127.0.0.1:8080:3000"];
  assertEquals(extractAppPort(ports), 3000);
});

Deno.test("extractAppPort - with protocol", () => {
  const ports = ["8080:80/tcp"];
  assertEquals(extractAppPort(ports), 80);
});

Deno.test("extractAppPort - complex mapping", () => {
  const ports = ["192.168.1.100:8080:3000/tcp"];
  assertEquals(extractAppPort(ports), 3000);
});

Deno.test("extractAppPort - complex mapping without protocol", () => {
  const ports = ["192.168.1.100:8080:4000"];
  assertEquals(extractAppPort(ports), 4000);
});

Deno.test("extractAppPort - host:container format", () => {
  const ports = ["8080:3000"];
  assertEquals(extractAppPort(ports), 3000);
});

Deno.test("extractAppPort - host:container format with protocol", () => {
  const ports = ["8080:3000/udp"];
  assertEquals(extractAppPort(ports), 3000);
});

Deno.test("extractAppPort - multiple ports", () => {
  const ports = ["8080:80", "8443:443"];
  // Should return the first port mapping
  assertEquals(extractAppPort(ports), 80);
});

Deno.test("extractAppPort - empty ports array", () => {
  const ports: string[] = [];
  assertEquals(extractAppPort(ports), 3000); // Default port
});

Deno.test("extractAppPort - single port", () => {
  const ports = ["80"];
  assertEquals(extractAppPort(ports), 80);
});

Deno.test("ProxyCommands - deploy path prefix with special characters", async () => {
  const mockSSH = new MockSSHManager();
  const proxyCommands = new ProxyCommands(
    "docker",
    mockSSH as unknown as SSHManager,
  );

  const config = new ProxyConfiguration({
    ssl: false,
    host: "special.example.com",
    path_prefix: "/api-v1/users_admin",
  });

  await proxyCommands.deploy(
    "special-service",
    "special-container",
    config,
    3000,
    "myproject",
  );

  const lastCommand = mockSSH.getLastCommand();
  assertStringIncludes(lastCommand, "--path-prefix=/api-v1/users_admin");
});

Deno.test("ProxyCommands - deploy root path prefix", async () => {
  const mockSSH = new MockSSHManager();
  const proxyCommands = new ProxyCommands(
    "docker",
    mockSSH as unknown as SSHManager,
  );

  const config = new ProxyConfiguration({
    ssl: false,
    host: "root.example.com",
    path_prefix: "/",
  });

  await proxyCommands.deploy(
    "root-service",
    "root-container",
    config,
    3000,
    "myproject",
  );

  const lastCommand = mockSSH.getLastCommand();
  assertStringIncludes(lastCommand, "--path-prefix=/");
});

Deno.test("ProxyCommands - deploy generates correct command format", async () => {
  const mockSSH = new MockSSHManager();
  const proxyCommands = new ProxyCommands(
    "podman",
    mockSSH as unknown as SSHManager,
  );

  const config = new ProxyConfiguration({
    ssl: true,
    host: "format.example.com",
    path_prefix: "/test",
  });

  await proxyCommands.deploy(
    "format-service",
    "format-container",
    config,
    4000,
    "myproject",
  );

  const lastCommand = mockSSH.getLastCommand();

  // Verify the command structure
  assertStringIncludes(
    lastCommand,
    "podman exec kamal-proxy kamal-proxy deploy format-service",
  );

  // Verify all options are present and in correct format
  const expectedParts = [
    "--target=myproject-format-service.jiji:4000",
    "--host=format.example.com",
    "--path-prefix=/test",
    "--tls",
  ];

  for (const part of expectedParts) {
    assertStringIncludes(lastCommand, part);
  }
});

// Unit tests for helper functions

Deno.test("buildKamalProxyOptions - creates complete options", () => {
  const config = new ProxyConfiguration({
    ssl: true,
    host: "example.com",
    path_prefix: "/api",
    healthcheck: { path: "/health", interval: "30s" },
  });

  const options = buildKamalProxyOptions(
    "api",
    "api-container",
    3000,
    config,
    "testproject",
  );

  assertEquals(options.serviceName, "api");
  assertEquals(options.target, "testproject-api.jiji:3000");
  assertEquals(options.hosts, ["example.com"]);
  assertEquals(options.pathPrefix, "/api");
  assertEquals(options.tls, true);
  assertEquals(options.healthCheckPath, "/health");
  assertEquals(options.healthCheckInterval, "30s");
});

Deno.test("buildKamalProxyOptions - minimal options", () => {
  const config = new ProxyConfiguration({
    ssl: false,
  });

  const options = buildKamalProxyOptions(
    "web",
    "web-container",
    8080,
    config,
    "testproject",
  );

  assertEquals(options.serviceName, "web");
  assertEquals(options.target, "testproject-web.jiji:8080");
  assertEquals(options.hosts, undefined);
  assertEquals(options.pathPrefix, undefined);
  assertEquals(options.tls, false);
  assertEquals(options.healthCheckPath, undefined);
  assertEquals(options.healthCheckInterval, undefined);
});

Deno.test("buildKamalProxyOptions - with partial healthcheck", () => {
  const config = new ProxyConfiguration({
    ssl: false,
    host: "test.com",
    healthcheck: { path: "/status" },
  });

  const options = buildKamalProxyOptions(
    "svc",
    "container",
    5000,
    config,
    "testproject",
  );

  assertEquals(options.serviceName, "svc");
  assertEquals(options.target, "testproject-svc.jiji:5000");
  assertEquals(options.hosts, ["test.com"]);
  assertEquals(options.healthCheckPath, "/status");
  assertEquals(options.healthCheckInterval, undefined);
});

Deno.test("buildDeployCommandArgs - builds correct argument array with all options", () => {
  const options: KamalProxyDeployOptions = {
    serviceName: "test",
    target: "container:3000",
    hosts: ["example.com"],
    pathPrefix: "/api",
    tls: true,
    healthCheckPath: "/health",
    healthCheckInterval: "30s",
  };

  const args = buildDeployCommandArgs(options);

  assertEquals(args.includes("--target=container:3000"), true);
  assertEquals(args.includes("--host=example.com"), true);
  assertEquals(args.includes("--path-prefix=/api"), true);
  assertEquals(args.includes("--tls"), true);
  assertEquals(args.includes("--health-check-path=/health"), true);
  assertEquals(args.includes("--health-check-interval=30s"), true);
});

Deno.test("buildDeployCommandArgs - minimal options", () => {
  const options: KamalProxyDeployOptions = {
    serviceName: "minimal",
    target: "min-container:8080",
  };

  const args = buildDeployCommandArgs(options);

  assertEquals(args.length, 1);
  assertEquals(args[0], "--target=min-container:8080");
});

Deno.test("buildDeployCommandArgs - with host and SSL only", () => {
  const options: KamalProxyDeployOptions = {
    serviceName: "secure",
    target: "secure-container:443",
    hosts: ["secure.example.com"],
    tls: true,
  };

  const args = buildDeployCommandArgs(options);

  assertEquals(args.includes("--target=secure-container:443"), true);
  assertEquals(args.includes("--host=secure.example.com"), true);
  assertEquals(args.includes("--tls"), true);
  assertEquals(args.includes("--path-prefix"), false);
  assertEquals(args.includes("--health-check-path"), false);
});

Deno.test("buildDeployCommandArgs - with path prefix only", () => {
  const options: KamalProxyDeployOptions = {
    serviceName: "path-service",
    target: "path-container:3000",
    pathPrefix: "/admin",
  };

  const args = buildDeployCommandArgs(options);

  assertEquals(args.includes("--target=path-container:3000"), true);
  assertEquals(args.includes("--path-prefix=/admin"), true);
  assertEquals(args.length, 2);
});

Deno.test("buildDeployCommandArgs - respects false tls value", () => {
  const options: KamalProxyDeployOptions = {
    serviceName: "no-tls",
    target: "container:3000",
    hosts: ["http.example.com"],
    tls: false,
  };

  const args = buildDeployCommandArgs(options);

  assertEquals(args.includes("--tls"), false);
  assertEquals(args.includes("--host=http.example.com"), true);
});

Deno.test("buildDeployCommandArgs - multiple hosts", () => {
  const options: KamalProxyDeployOptions = {
    serviceName: "multi-host",
    target: "container:3000",
    hosts: ["domain.com", "other.domain.com", "www.domain.com"],
    tls: true,
  };

  const args = buildDeployCommandArgs(options);

  assertEquals(args.includes("--target=container:3000"), true);
  assertEquals(args.includes("--host=domain.com"), true);
  assertEquals(args.includes("--host=other.domain.com"), true);
  assertEquals(args.includes("--host=www.domain.com"), true);
  assertEquals(args.includes("--tls"), true);

  // Verify all three hosts are present in the args
  const hostArgs = args.filter((arg) => arg.startsWith("--host="));
  assertEquals(hostArgs.length, 3);
});

Deno.test("ProxyConfiguration - supports hosts array", () => {
  const config = new ProxyConfiguration({
    ssl: true,
    hosts: ["domain.com", "www.domain.com"],
  });

  assertEquals(config.hosts, ["domain.com", "www.domain.com"]);
  assertEquals(config.enabled, true);
  assertEquals(config.ssl, true);
});
