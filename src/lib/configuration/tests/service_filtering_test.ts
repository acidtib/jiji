import { assertEquals } from "@std/assert";
import { Configuration } from "../../configuration.ts";

Deno.test("Configuration - getHostsFromServices with exact match", () => {
  const config = new Configuration({
    project: "test",
    engine: "docker",
    ssh: { user: "root" },
    services: {
      web: {
        image: "nginx",
        servers: [
          { host: "192.168.1.10" },
          { host: "192.168.1.11" },
        ],
      },
      api: {
        image: "node",
        servers: [{ host: "192.168.1.20" }],
      },
      database: {
        image: "postgres",
        servers: [{ host: "192.168.1.30" }],
      },
    },
  });

  const hosts = config.getHostsFromServices(["web"]);
  assertEquals(hosts, ["192.168.1.10", "192.168.1.11"]);
});

Deno.test("Configuration - getHostsFromServices with multiple services", () => {
  const config = new Configuration({
    project: "test",
    engine: "docker",
    ssh: { user: "root" },
    services: {
      web: {
        image: "nginx",
        servers: [{ host: "192.168.1.10" }],
      },
      api: {
        image: "node",
        servers: [{ host: "192.168.1.20" }],
      },
      worker: {
        image: "worker",
        servers: [{ host: "192.168.1.30" }],
      },
    },
  });

  const hosts = config.getHostsFromServices(["web", "api"]);
  assertEquals(hosts, ["192.168.1.10", "192.168.1.20"]);
});

Deno.test("Configuration - getHostsFromServices with wildcard pattern", () => {
  const config = new Configuration({
    project: "test",
    engine: "docker",
    ssh: { user: "root" },
    services: {
      "web-frontend": {
        image: "nginx",
        servers: [{ host: "192.168.1.10" }],
      },
      "web-api": {
        image: "node",
        servers: [{ host: "192.168.1.20" }],
      },
      "web-worker": {
        image: "worker",
        servers: [{ host: "192.168.1.30" }],
      },
      database: {
        image: "postgres",
        servers: [{ host: "192.168.1.40" }],
      },
    },
  });

  const hosts = config.getHostsFromServices(["web-*"]);
  assertEquals(hosts, ["192.168.1.10", "192.168.1.20", "192.168.1.30"]);
});

Deno.test("Configuration - getHostsFromServices removes duplicates", () => {
  const config = new Configuration({
    project: "test",
    engine: "docker",
    ssh: { user: "root" },
    services: {
      web: {
        image: "nginx",
        servers: [
          { host: "192.168.1.10" },
          { host: "192.168.1.20" },
        ],
      },
      api: {
        image: "node",
        servers: [
          { host: "192.168.1.20" },
          { host: "192.168.1.30" },
        ],
      },
    },
  });

  const hosts = config.getHostsFromServices(["web", "api"]);
  // Should include 192.168.1.20 only once
  assertEquals(hosts, ["192.168.1.10", "192.168.1.20", "192.168.1.30"]);
});

Deno.test("Configuration - getHostsFromServices with no matches", () => {
  const config = new Configuration({
    project: "test",
    engine: "docker",
    ssh: { user: "root" },
    services: {
      web: {
        image: "nginx",
        servers: [{ host: "192.168.1.10" }],
      },
    },
  });

  const hosts = config.getHostsFromServices(["nonexistent"]);
  assertEquals(hosts, []);
});

Deno.test("Configuration - getHostsFromServices with mixed patterns", () => {
  const config = new Configuration({
    project: "test",
    engine: "docker",
    ssh: { user: "root" },
    services: {
      "web-frontend": {
        image: "nginx",
        servers: [{ host: "192.168.1.10" }],
      },
      "web-api": {
        image: "node",
        servers: [{ host: "192.168.1.20" }],
      },
      database: {
        image: "postgres",
        servers: [{ host: "192.168.1.30" }],
      },
      cache: {
        image: "redis",
        servers: [{ host: "192.168.1.40" }],
      },
    },
  });

  const hosts = config.getHostsFromServices(["web-*", "database"]);
  assertEquals(hosts, ["192.168.1.10", "192.168.1.20", "192.168.1.30"]);
});

Deno.test("Configuration - getMatchingServiceNames with exact match", () => {
  const config = new Configuration({
    project: "test",
    engine: "docker",
    ssh: { user: "root" },
    services: {
      web: { image: "nginx", servers: [{ host: "192.168.1.10" }] },
      api: { image: "node", servers: [{ host: "192.168.1.20" }] },
    },
  });

  const services = config.getMatchingServiceNames(["web"]);
  assertEquals(services, ["web"]);
});

Deno.test("Configuration - getMatchingServiceNames with wildcard", () => {
  const config = new Configuration({
    project: "test",
    engine: "docker",
    ssh: { user: "root" },
    services: {
      "web-frontend": { image: "nginx", servers: [{ host: "192.168.1.10" }] },
      "web-api": { image: "node", servers: [{ host: "192.168.1.20" }] },
      "api-worker": { image: "worker", servers: [{ host: "192.168.1.30" }] },
    },
  });

  const services = config.getMatchingServiceNames(["web-*"]);
  assertEquals(services, ["web-api", "web-frontend"]);
});

Deno.test("Configuration - getMatchingServiceNames with multiple patterns", () => {
  const config = new Configuration({
    project: "test",
    engine: "docker",
    ssh: { user: "root" },
    services: {
      "web-frontend": { image: "nginx", servers: [{ host: "192.168.1.10" }] },
      "web-api": { image: "node", servers: [{ host: "192.168.1.20" }] },
      "api-worker": { image: "worker", servers: [{ host: "192.168.1.30" }] },
      "api-processor": {
        image: "processor",
        servers: [{ host: "192.168.1.40" }],
      },
    },
  });

  const services = config.getMatchingServiceNames(["web-*", "api-*"]);
  assertEquals(
    services,
    ["api-processor", "api-worker", "web-api", "web-frontend"],
  );
});

Deno.test("Configuration - getMatchingServiceNames with no matches", () => {
  const config = new Configuration({
    project: "test",
    engine: "docker",
    ssh: { user: "root" },
    services: {
      web: { image: "nginx", servers: [{ host: "192.168.1.10" }] },
    },
  });

  const services = config.getMatchingServiceNames(["nonexistent"]);
  assertEquals(services, []);
});

Deno.test("Configuration - getMatchingServiceNames removes duplicates", () => {
  const config = new Configuration({
    project: "test",
    engine: "docker",
    ssh: { user: "root" },
    services: {
      "web-api": { image: "node", servers: [{ host: "192.168.1.10" }] },
      "web-frontend": { image: "nginx", servers: [{ host: "192.168.1.20" }] },
    },
  });

  // Both patterns match "web-api"
  const services = config.getMatchingServiceNames(["web-api", "web-*"]);
  // Should only include "web-api" once
  assertEquals(services, ["web-api", "web-frontend"]);
});

Deno.test("Configuration - service filtering supports ? wildcard", () => {
  const config = new Configuration({
    project: "test",
    engine: "docker",
    ssh: { user: "root" },
    services: {
      web1: { image: "nginx", servers: [{ host: "192.168.1.10" }] },
      web2: { image: "nginx", servers: [{ host: "192.168.1.20" }] },
      web12: { image: "nginx", servers: [{ host: "192.168.1.30" }] },
    },
  });

  // "web?" matches web1 and web2 but not web12 (? matches exactly one character)
  const services = config.getMatchingServiceNames(["web?"]);
  assertEquals(services, ["web1", "web2"]);
});

Deno.test("Configuration - service filtering is case-sensitive", () => {
  const config = new Configuration({
    project: "test",
    engine: "docker",
    ssh: { user: "root" },
    services: {
      web: { image: "nginx", servers: [{ host: "192.168.1.10" }] },
      Web: { image: "nginx", servers: [{ host: "192.168.1.20" }] },
      WEB: { image: "nginx", servers: [{ host: "192.168.1.30" }] },
    },
  });

  const services = config.getMatchingServiceNames(["web"]);
  assertEquals(services, ["web"]);
});
