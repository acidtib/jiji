import { assertEquals, assertThrows } from "@std/assert";
import { ProxyConfiguration } from "../proxy.ts";
import { ConfigurationError } from "../base.ts";

Deno.test("ProxyConfiguration - basic host configuration", () => {
  const config = new ProxyConfiguration({
    ssl: true,
    host: "example.com",
  });

  assertEquals(config.ssl, true);
  assertEquals(config.host, "example.com");
  assertEquals(config.pathPrefix, undefined);
  assertEquals(config.enabled, true);
});

Deno.test("ProxyConfiguration - host with path prefix", () => {
  const config = new ProxyConfiguration({
    ssl: false,
    host: "api.example.com",
    path_prefix: "/v1",
  });

  assertEquals(config.ssl, false);
  assertEquals(config.host, "api.example.com");
  assertEquals(config.pathPrefix, "/v1");
  assertEquals(config.enabled, true);
});

Deno.test("ProxyConfiguration - path prefix only (no host)", () => {
  const config = new ProxyConfiguration({
    ssl: true,
    path_prefix: "/api",
  });

  assertEquals(config.ssl, true);
  assertEquals(config.host, undefined);
  assertEquals(config.pathPrefix, "/api");
  assertEquals(config.enabled, false); // No host means proxy is disabled
});

Deno.test("ProxyConfiguration - healthcheck configuration", () => {
  const config = new ProxyConfiguration({
    host: "example.com",
    healthcheck: {
      path: "/health",
      interval: "30s",
    },
  });

  assertEquals(config.host, "example.com");
  assertEquals(config.healthcheck?.path, "/health");
  assertEquals(config.healthcheck?.interval, "30s");
});

Deno.test("ProxyConfiguration - empty configuration", () => {
  const config = new ProxyConfiguration({});

  assertEquals(config.ssl, false);
  assertEquals(config.host, undefined);
  assertEquals(config.pathPrefix, undefined);
  assertEquals(config.enabled, false);
  assertEquals(config.healthcheck, undefined);
});

Deno.test("ProxyConfiguration - validate with valid host", () => {
  const config = new ProxyConfiguration({
    ssl: true,
    host: "api.example.com",
    path_prefix: "/v1",
    healthcheck: {
      path: "/health",
      interval: "10s",
    },
  });

  // Should not throw
  config.validate();
});

Deno.test("ProxyConfiguration - validate invalid host format", () => {
  const config = new ProxyConfiguration({
    host: "invalid..host",
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "Invalid host format: invalid..host",
  );
});

Deno.test("ProxyConfiguration - validate localhost warning", () => {
  const config = new ProxyConfiguration({
    host: "localhost",
  });

  // Should not throw but should warn (we can't easily test console.warn)
  config.validate();
});

Deno.test("ProxyConfiguration - validate SSL without host", () => {
  const config = new ProxyConfiguration({
    ssl: true,
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "SSL requires a host to be configured",
  );
});

Deno.test("ProxyConfiguration - validate path prefix without leading slash", () => {
  const config = new ProxyConfiguration({
    host: "example.com",
    path_prefix: "api",
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "Path prefix must start with /: api",
  );
});

Deno.test("ProxyConfiguration - validate path prefix with invalid characters", () => {
  const config = new ProxyConfiguration({
    host: "example.com",
    path_prefix: "/api<script>",
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "Invalid characters in path prefix: /api<script>",
  );
});

Deno.test("ProxyConfiguration - validate root path prefix", () => {
  const config = new ProxyConfiguration({
    host: "example.com",
    path_prefix: "/",
  });

  // Should not throw or warn for root path
  config.validate();
});

Deno.test("ProxyConfiguration - validate path prefix with trailing slash warning", () => {
  const config = new ProxyConfiguration({
    host: "example.com",
    path_prefix: "/api/",
  });

  // Should not throw but should warn (we can't easily test console.warn)
  config.validate();
});

Deno.test("ProxyConfiguration - validate healthcheck path without leading slash", () => {
  const config = new ProxyConfiguration({
    host: "example.com",
    healthcheck: {
      path: "health",
    },
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "Health check path must start with /: health",
  );
});

Deno.test("ProxyConfiguration - validate healthcheck path with invalid characters", () => {
  const config = new ProxyConfiguration({
    host: "example.com",
    healthcheck: {
      path: "/health?<script>",
    },
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "Health check path contains invalid characters: /health?<script>",
  );
});

Deno.test("ProxyConfiguration - validate healthcheck interval format", () => {
  const config = new ProxyConfiguration({
    host: "example.com",
    healthcheck: {
      interval: "invalid",
    },
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "Invalid health check interval: invalid",
  );
});

Deno.test("ProxyConfiguration - validate healthcheck interval too short", () => {
  const config = new ProxyConfiguration({
    host: "example.com",
    healthcheck: {
      interval: "0s",
    },
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "Health check interval too short: 0s. Minimum is 1s.",
  );
});

Deno.test("ProxyConfiguration - validate various time formats", () => {
  const validIntervals = ["1s", "30s", "5m", "1h"];

  for (const interval of validIntervals) {
    const config = new ProxyConfiguration({
      host: "example.com",
      healthcheck: {
        interval,
      },
    });

    // Should not throw
    config.validate();
  }
});

Deno.test("ProxyConfiguration - complex valid configuration", () => {
  const config = new ProxyConfiguration({
    ssl: true,
    host: "api.myproject.example.com",
    path_prefix: "/v2/graphql",
    healthcheck: {
      path: "/v2/graphql/health",
      interval: "15s",
    },
  });

  assertEquals(config.ssl, true);
  assertEquals(config.host, "api.myproject.example.com");
  assertEquals(config.pathPrefix, "/v2/graphql");
  assertEquals(config.enabled, true);
  assertEquals(config.healthcheck?.path, "/v2/graphql/health");
  assertEquals(config.healthcheck?.interval, "15s");

  // Should validate successfully
  config.validate();
});

Deno.test("ProxyConfiguration - invalid healthcheck object", () => {
  const config = new ProxyConfiguration({
    host: "example.com",
    healthcheck: "not-an-object",
  });

  assertEquals(config.healthcheck, undefined);
});

Deno.test("ProxyConfiguration - non-string path_prefix", () => {
  const config = new ProxyConfiguration({
    host: "example.com",
    path_prefix: 123,
  });

  assertEquals(config.pathPrefix, undefined);
});

Deno.test("ProxyConfiguration - non-string host", () => {
  const config = new ProxyConfiguration({
    host: 123,
  });

  assertEquals(config.host, undefined);
  assertEquals(config.enabled, false);
});

Deno.test("ProxyConfiguration - non-boolean ssl", () => {
  const config = new ProxyConfiguration({
    host: "example.com",
    ssl: "true",
  });

  assertEquals(config.ssl, false); // Should default to false for non-boolean
});
