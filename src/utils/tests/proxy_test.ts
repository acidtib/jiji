import { assertEquals, assertStringIncludes } from "@std/assert";
import { extractAppPort, ProxyCommands } from "../proxy.ts";
import { ProxyConfiguration } from "../../lib/configuration/proxy.ts";
import { MockSSHManager } from "../../../tests/mocks.ts";

Deno.test("ProxyCommands - deploy with host only", async () => {
  const mockSSH = new MockSSHManager();
  const proxyCommands = new ProxyCommands("docker", mockSSH as any);

  const config = new ProxyConfiguration({
    ssl: false,
    host: "example.com",
  });

  await proxyCommands.deploy("test-service", "test-container", config, 3000);

  const lastCommand = mockSSH.getLastCommand();
  assertStringIncludes(lastCommand, "kamal-proxy deploy test-service");
  assertStringIncludes(lastCommand, "--target=test-container:3000");
  assertStringIncludes(lastCommand, "--host=example.com");
  // Should not include SSL or path prefix
  assertEquals(lastCommand.includes("--tls"), false);
  assertEquals(lastCommand.includes("--path-prefix"), false);
});

Deno.test("ProxyCommands - deploy with SSL enabled", async () => {
  const mockSSH = new MockSSHManager();
  const proxyCommands = new ProxyCommands("docker", mockSSH as any);

  const config = new ProxyConfiguration({
    ssl: true,
    host: "secure.example.com",
  });

  await proxyCommands.deploy(
    "secure-service",
    "secure-container",
    config,
    8080,
  );

  const lastCommand = mockSSH.getLastCommand();
  assertStringIncludes(lastCommand, "kamal-proxy deploy secure-service");
  assertStringIncludes(lastCommand, "--target=secure-container:8080");
  assertStringIncludes(lastCommand, "--host=secure.example.com");
  assertStringIncludes(lastCommand, "--tls");
});

Deno.test("ProxyCommands - deploy with path prefix", async () => {
  const mockSSH = new MockSSHManager();
  const proxyCommands = new ProxyCommands("docker", mockSSH as any);

  const config = new ProxyConfiguration({
    ssl: false,
    host: "api.example.com",
    path_prefix: "/v1",
  });

  await proxyCommands.deploy("api-service", "api-container", config, 4000);

  const lastCommand = mockSSH.getLastCommand();
  assertStringIncludes(lastCommand, "kamal-proxy deploy api-service");
  assertStringIncludes(lastCommand, "--target=api-container:4000");
  assertStringIncludes(lastCommand, "--host=api.example.com");
  assertStringIncludes(lastCommand, "--path-prefix=/v1");
});

Deno.test("ProxyCommands - deploy with complex path prefix", async () => {
  const mockSSH = new MockSSHManager();
  const proxyCommands = new ProxyCommands("docker", mockSSH as any);

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
  );

  const lastCommand = mockSSH.getLastCommand();
  assertStringIncludes(lastCommand, "kamal-proxy deploy graphql-service");
  assertStringIncludes(lastCommand, "--target=graphql-container:5000");
  assertStringIncludes(lastCommand, "--host=app.example.com");
  assertStringIncludes(lastCommand, "--path-prefix=/api/v2/graphql");
  assertStringIncludes(lastCommand, "--tls");
});

Deno.test("ProxyCommands - deploy with all options", async () => {
  const mockSSH = new MockSSHManager();
  const proxyCommands = new ProxyCommands("podman", mockSSH as any);

  const config = new ProxyConfiguration({
    ssl: true,
    host: "full.example.com",
    path_prefix: "/admin",
    healthcheck: {
      path: "/admin/health",
      interval: "30s",
    },
  });

  await proxyCommands.deploy("admin-service", "admin-container", config, 3001);

  const lastCommand = mockSSH.getLastCommand();
  assertStringIncludes(lastCommand, "podman exec kamal-proxy");
  assertStringIncludes(lastCommand, "kamal-proxy deploy admin-service");
  assertStringIncludes(lastCommand, "--target=admin-container:3001");
  assertStringIncludes(lastCommand, "--host=full.example.com");
  assertStringIncludes(lastCommand, "--path-prefix=/admin");
  assertStringIncludes(lastCommand, "--tls");
  assertStringIncludes(lastCommand, "--health-check-path=/admin/health");
  assertStringIncludes(lastCommand, "--health-check-interval=30s");
});

Deno.test("ProxyCommands - deploy with health check only", async () => {
  const mockSSH = new MockSSHManager();
  const proxyCommands = new ProxyCommands("docker", mockSSH as any);

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
  );

  const lastCommand = mockSSH.getLastCommand();
  assertStringIncludes(lastCommand, "kamal-proxy deploy health-service");
  assertStringIncludes(lastCommand, "--target=health-container:8000");
  assertStringIncludes(lastCommand, "--host=health.example.com");
  assertStringIncludes(lastCommand, "--health-check-path=/status");
  assertStringIncludes(lastCommand, "--health-check-interval=10s");
  assertEquals(lastCommand.includes("--tls"), false);
  assertEquals(lastCommand.includes("--path-prefix"), false);
});

Deno.test("ProxyCommands - deploy with partial health check", async () => {
  const mockSSH = new MockSSHManager();
  const proxyCommands = new ProxyCommands("docker", mockSSH as any);

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
  );

  const lastCommand = mockSSH.getLastCommand();
  assertStringIncludes(lastCommand, "--health-check-path=/ping");
  assertEquals(lastCommand.includes("--health-check-interval"), false);
});

Deno.test("ProxyCommands - deploy command failure", async () => {
  const mockSSH = new MockSSHManager(false); // Configure to fail
  const proxyCommands = new ProxyCommands("docker", mockSSH as any);

  const config = new ProxyConfiguration({
    ssl: false,
    host: "fail.example.com",
  });

  let errorThrown = false;
  try {
    await proxyCommands.deploy("fail-service", "fail-container", config, 3000);
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
  const proxyCommands = new ProxyCommands("docker", mockSSH as any);

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
  );

  const lastCommand = mockSSH.getLastCommand();
  assertStringIncludes(lastCommand, "--path-prefix=/api-v1/users_admin");
});

Deno.test("ProxyCommands - deploy root path prefix", async () => {
  const mockSSH = new MockSSHManager();
  const proxyCommands = new ProxyCommands("docker", mockSSH as any);

  const config = new ProxyConfiguration({
    ssl: false,
    host: "root.example.com",
    path_prefix: "/",
  });

  await proxyCommands.deploy("root-service", "root-container", config, 3000);

  const lastCommand = mockSSH.getLastCommand();
  assertStringIncludes(lastCommand, "--path-prefix=/");
});

Deno.test("ProxyCommands - deploy generates correct command format", async () => {
  const mockSSH = new MockSSHManager();
  const proxyCommands = new ProxyCommands("podman", mockSSH as any);

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
  );

  const lastCommand = mockSSH.getLastCommand();

  // Verify the command structure
  assertStringIncludes(
    lastCommand,
    "podman exec kamal-proxy kamal-proxy deploy format-service",
  );

  // Verify all options are present and in correct format
  const expectedParts = [
    "--target=format-container:4000",
    "--host=format.example.com",
    "--path-prefix=/test",
    "--tls",
  ];

  for (const part of expectedParts) {
    assertStringIncludes(lastCommand, part);
  }
});
