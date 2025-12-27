import { assertEquals, assertThrows } from "@std/assert";
import { ProxyConfiguration } from "../proxy.ts";
import { ConfigurationError } from "../base.ts";

// ============================================================================
// Single Target Configuration Tests
// ============================================================================

Deno.test("ProxyConfiguration - single target with port", () => {
  const config = new ProxyConfiguration({
    app_port: 3000,
    host: "example.com",
    ssl: true,
  });

  assertEquals(config.isMultiTarget, false);
  assertEquals(config.targets.length, 1);
  assertEquals(config.targets[0].app_port, 3000);
  assertEquals(config.targets[0].host, "example.com");
  assertEquals(config.targets[0].ssl, true);
  assertEquals(config.enabled, true);
  config.validate();
});

Deno.test("ProxyConfiguration - single target with path prefix", () => {
  const config = new ProxyConfiguration({
    app_port: 8080,
    host: "api.example.com",
    path_prefix: "/v1",
    ssl: false,
  });

  assertEquals(config.targets[0].app_port, 8080);
  assertEquals(config.targets[0].host, "api.example.com");
  assertEquals(config.targets[0].path_prefix, "/v1");
  assertEquals(config.targets[0].ssl, false);
  config.validate();
});

Deno.test("ProxyConfiguration - single target with healthcheck", () => {
  const config = new ProxyConfiguration({
    app_port: 3000,
    host: "example.com",
    healthcheck: {
      path: "/health",
      interval: "30s",
      timeout: "5s",
      deploy_timeout: "60s",
    },
  });

  assertEquals(config.targets[0].healthcheck?.path, "/health");
  assertEquals(config.targets[0].healthcheck?.interval, "30s");
  assertEquals(config.targets[0].healthcheck?.timeout, "5s");
  assertEquals(config.targets[0].healthcheck?.deploy_timeout, "60s");
  config.validate();
});

Deno.test("ProxyConfiguration - single target with hosts array", () => {
  const config = new ProxyConfiguration({
    app_port: 3000,
    hosts: ["api.example.com", "api2.example.com"],
    ssl: true,
  });

  assertEquals(config.targets[0].hosts, [
    "api.example.com",
    "api2.example.com",
  ]);
  assertEquals(config.targets[0].host, undefined);
  config.validate();
});

Deno.test("ProxyConfiguration - single target missing port", () => {
  const config = new ProxyConfiguration({
    host: "example.com",
    ssl: true,
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "proxy must specify an 'app_port' number",
  );
});

Deno.test("ProxyConfiguration - single target missing host", () => {
  const config = new ProxyConfiguration({
    app_port: 3000,
    ssl: true,
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "proxy must specify either 'host' or 'hosts'",
  );
});

Deno.test("ProxyConfiguration - single target with both host and hosts", () => {
  const config = new ProxyConfiguration({
    app_port: 3000,
    host: "example.com",
    hosts: ["api.example.com"],
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "proxy: use either 'host' or 'hosts', not both",
  );
});

// ============================================================================
// Multi-Target Configuration Tests
// ============================================================================

Deno.test("ProxyConfiguration - multi-target with two ports", () => {
  const config = new ProxyConfiguration({
    targets: [
      { app_port: 3900, host: "s3.example.com", ssl: false },
      { app_port: 3903, host: "admin.example.com", ssl: true },
    ],
  });

  assertEquals(config.isMultiTarget, true);
  assertEquals(config.targets.length, 2);
  assertEquals(config.targets[0].app_port, 3900);
  assertEquals(config.targets[0].host, "s3.example.com");
  assertEquals(config.targets[0].ssl, false);
  assertEquals(config.targets[1].app_port, 3903);
  assertEquals(config.targets[1].host, "admin.example.com");
  assertEquals(config.targets[1].ssl, true);
  assertEquals(config.enabled, true);
  config.validate();
});

Deno.test("ProxyConfiguration - multi-target with healthchecks", () => {
  const config = new ProxyConfiguration({
    targets: [
      {
        app_port: 3900,
        host: "s3.example.com",
        healthcheck: {
          path: "/health",
          interval: "10s",
        },
      },
      {
        app_port: 3903,
        host: "admin.example.com",
        healthcheck: {
          path: "/admin/health",
          interval: "15s",
          deploy_timeout: "30s",
        },
      },
    ],
  });

  assertEquals(config.targets[0].healthcheck?.path, "/health");
  assertEquals(config.targets[0].healthcheck?.interval, "10s");
  assertEquals(config.targets[1].healthcheck?.path, "/admin/health");
  assertEquals(config.targets[1].healthcheck?.interval, "15s");
  assertEquals(config.targets[1].healthcheck?.deploy_timeout, "30s");
  config.validate();
});

Deno.test("ProxyConfiguration - multi-target with path prefixes", () => {
  const config = new ProxyConfiguration({
    targets: [
      { app_port: 3000, host: "example.com", path_prefix: "/api" },
      { app_port: 3001, host: "example.com", path_prefix: "/admin" },
    ],
  });

  assertEquals(config.targets[0].path_prefix, "/api");
  assertEquals(config.targets[1].path_prefix, "/admin");
  config.validate();
});

Deno.test("ProxyConfiguration - multi-target with hosts arrays", () => {
  const config = new ProxyConfiguration({
    targets: [
      { app_port: 3000, hosts: ["api.example.com", "api2.example.com"] },
      { app_port: 3001, hosts: ["admin.example.com"] },
    ],
  });

  assertEquals(config.targets[0].hosts, [
    "api.example.com",
    "api2.example.com",
  ]);
  assertEquals(config.targets[1].hosts, ["admin.example.com"]);
  config.validate();
});

Deno.test("ProxyConfiguration - empty targets array", () => {
  const config = new ProxyConfiguration({
    targets: [],
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "'targets' array cannot be empty",
  );
});

Deno.test("ProxyConfiguration - target missing port", () => {
  const config = new ProxyConfiguration({
    targets: [
      { host: "example.com" },
    ],
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "target at index 0 must specify an 'app_port' number",
  );
});

Deno.test("ProxyConfiguration - target missing host", () => {
  const config = new ProxyConfiguration({
    targets: [
      { app_port: 3000 },
    ],
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "target at index 0 must specify either 'host' or 'hosts'",
  );
});

Deno.test("ProxyConfiguration - target with both host and hosts", () => {
  const config = new ProxyConfiguration({
    targets: [
      { app_port: 3000, host: "example.com", hosts: ["api.example.com"] },
    ],
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "target at index 0: use either 'host' or 'hosts', not both",
  );
});

// ============================================================================
// Validation Tests
// ============================================================================

Deno.test("ProxyConfiguration - invalid host format in single target", () => {
  const config = new ProxyConfiguration({
    app_port: 3000,
    host: "invalid..host",
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "Invalid host format in proxy: invalid..host",
  );
});

Deno.test("ProxyConfiguration - invalid host format in multi-target", () => {
  const config = new ProxyConfiguration({
    targets: [
      { app_port: 3000, host: "invalid..host" },
    ],
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "Invalid host format in target at index 0: invalid..host",
  );
});

Deno.test("ProxyConfiguration - path prefix without leading slash", () => {
  const config = new ProxyConfiguration({
    app_port: 3000,
    host: "example.com",
    path_prefix: "api",
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "Path prefix in proxy must start with '/': api",
  );
});

Deno.test("ProxyConfiguration - path prefix with invalid characters", () => {
  const config = new ProxyConfiguration({
    app_port: 3000,
    host: "example.com",
    path_prefix: "/api<script>",
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "Path prefix in proxy contains invalid characters: /api<script>",
  );
});

Deno.test("ProxyConfiguration - healthcheck path without leading slash", () => {
  const config = new ProxyConfiguration({
    app_port: 3000,
    host: "example.com",
    healthcheck: {
      path: "health",
    },
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "Health check path in proxy must start with /: health",
  );
});

Deno.test("ProxyConfiguration - healthcheck path with invalid characters", () => {
  const config = new ProxyConfiguration({
    app_port: 3000,
    host: "example.com",
    healthcheck: {
      path: "/health?<script>",
    },
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "Health check path in proxy contains invalid characters: /health?<script>",
  );
});

Deno.test("ProxyConfiguration - invalid healthcheck interval format", () => {
  const config = new ProxyConfiguration({
    app_port: 3000,
    host: "example.com",
    healthcheck: {
      interval: "invalid",
    },
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "Invalid health check interval in proxy: invalid",
  );
});

Deno.test("ProxyConfiguration - healthcheck interval too short", () => {
  const config = new ProxyConfiguration({
    app_port: 3000,
    host: "example.com",
    healthcheck: {
      interval: "0s",
    },
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "Health check interval too short in proxy: 0s. Minimum is 1s.",
  );
});

Deno.test("ProxyConfiguration - valid time formats", () => {
  const validIntervals = ["1s", "30s", "5m", "1h"];

  for (const interval of validIntervals) {
    const config = new ProxyConfiguration({
      app_port: 3000,
      host: "example.com",
      healthcheck: {
        interval,
      },
    });

    // Should not throw
    config.validate();
  }
});

Deno.test("ProxyConfiguration - invalid healthcheck timeout format", () => {
  const config = new ProxyConfiguration({
    app_port: 3000,
    host: "example.com",
    healthcheck: {
      timeout: "invalid",
    },
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "Invalid health check timeout in proxy: invalid",
  );
});

Deno.test("ProxyConfiguration - invalid deploy timeout format", () => {
  const config = new ProxyConfiguration({
    app_port: 3000,
    host: "example.com",
    healthcheck: {
      deploy_timeout: "invalid",
    },
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "Invalid deploy timeout in proxy: invalid",
  );
});

// ============================================================================
// Complex Scenarios
// ============================================================================

Deno.test("ProxyConfiguration - complex multi-target configuration", () => {
  const config = new ProxyConfiguration({
    targets: [
      {
        app_port: 3900,
        host: "s3.garage.example.com",
        ssl: false,
        healthcheck: {
          path: "/health",
          interval: "10s",
        },
      },
      {
        app_port: 3903,
        host: "admin.garage.example.com",
        ssl: true,
        path_prefix: "/admin",
        healthcheck: {
          path: "/admin/health",
          interval: "15s",
          timeout: "5s",
          deploy_timeout: "60s",
        },
      },
      {
        app_port: 8080,
        hosts: ["api1.example.com", "api2.example.com"],
        ssl: true,
      },
    ],
  });

  assertEquals(config.isMultiTarget, true);
  assertEquals(config.targets.length, 3);
  assertEquals(config.enabled, true);

  // Validate all targets
  assertEquals(config.targets[0].app_port, 3900);
  assertEquals(config.targets[0].host, "s3.garage.example.com");
  assertEquals(config.targets[0].ssl, false);

  assertEquals(config.targets[1].app_port, 3903);
  assertEquals(config.targets[1].host, "admin.garage.example.com");
  assertEquals(config.targets[1].ssl, true);
  assertEquals(config.targets[1].path_prefix, "/admin");

  assertEquals(config.targets[2].app_port, 8080);
  assertEquals(config.targets[2].hosts, [
    "api1.example.com",
    "api2.example.com",
  ]);
  assertEquals(config.targets[2].ssl, true);

  // Should validate successfully
  config.validate();
});

Deno.test("ProxyConfiguration - non-array targets value", () => {
  const config = new ProxyConfiguration({
    targets: "not-an-array",
  });

  // Should parse as empty array and fail validation
  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "'targets' array cannot be empty",
  );
});

Deno.test("ProxyConfiguration - localhost warning in single target", () => {
  const config = new ProxyConfiguration({
    app_port: 3000,
    host: "localhost",
  });

  // Should not throw but will warn (we can't easily test console.warn)
  config.validate();
});

Deno.test("ProxyConfiguration - 127.0.0.1 warning in multi-target", () => {
  const config = new ProxyConfiguration({
    targets: [
      { app_port: 3000, host: "127.0.0.1" },
    ],
  });

  // Should not throw but will warn (we can't easily test console.warn)
  config.validate();
});

Deno.test("ProxyConfiguration - enabled check with no targets", () => {
  const config = new ProxyConfiguration({
    app_port: 3000,
  });

  assertEquals(config.enabled, false);
});

Deno.test("ProxyConfiguration - enabled check with single target", () => {
  const config = new ProxyConfiguration({
    app_port: 3000,
    host: "example.com",
  });

  assertEquals(config.enabled, true);
});

Deno.test("ProxyConfiguration - enabled check with multi-target", () => {
  const config = new ProxyConfiguration({
    targets: [
      { app_port: 3000, host: "example.com" },
      { app_port: 3001, host: "api.example.com" },
    ],
  });

  assertEquals(config.enabled, true);
});
