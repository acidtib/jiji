import { assertEquals } from "@std/assert";
import { Configuration } from "../../configuration.ts";

Deno.test("Configuration - getHostsFromServices with exact match", () => {
  const config = new Configuration({
    project: "test",
    engine: "docker",
    ssh: { user: "root" },
    servers: {
      web1: { host: "192.168.1.10", arch: "amd64" },
      web2: { host: "192.168.1.11", arch: "amd64" },
      api1: { host: "192.168.1.20", arch: "amd64" },
      db1: { host: "192.168.1.30", arch: "amd64" },
    },
    services: {
      web: {
        image: "nginx",
        hosts: ["web1", "web2"],
      },
      api: {
        image: "node",
        hosts: ["api1"],
      },
      database: {
        image: "postgres",
        hosts: ["db1"],
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
    servers: {
      web1: { host: "192.168.1.10", arch: "amd64" },
      api1: { host: "192.168.1.20", arch: "amd64" },
      worker1: { host: "192.168.1.30", arch: "amd64" },
    },
    services: {
      web: {
        image: "nginx",
        hosts: ["web1"],
      },
      api: {
        image: "node",
        hosts: ["api1"],
      },
      worker: {
        image: "worker",
        hosts: ["worker1"],
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
    servers: {
      web1: { host: "192.168.1.10", arch: "amd64" },
      web2: { host: "192.168.1.20", arch: "amd64" },
      web3: { host: "192.168.1.30", arch: "amd64" },
      db1: { host: "192.168.1.40", arch: "amd64" },
    },
    services: {
      "web-frontend": {
        image: "nginx",
        hosts: ["web1"],
      },
      "web-api": {
        image: "node",
        hosts: ["web2"],
      },
      "web-worker": {
        image: "worker",
        hosts: ["web3"],
      },
      database: {
        image: "postgres",
        hosts: ["db1"],
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
    servers: {
      server1: { host: "192.168.1.10", arch: "amd64" },
      server2: { host: "192.168.1.20", arch: "amd64" },
      server3: { host: "192.168.1.30", arch: "amd64" },
    },
    services: {
      web: {
        image: "nginx",
        hosts: ["server1", "server2"],
      },
      api: {
        image: "node",
        hosts: ["server2", "server3"],
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
    servers: {
      web1: { host: "192.168.1.10", arch: "amd64" },
    },
    services: {
      web: {
        image: "nginx",
        hosts: ["web1"],
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
    servers: {
      web1: { host: "192.168.1.10", arch: "amd64" },
      web2: { host: "192.168.1.20", arch: "amd64" },
      db1: { host: "192.168.1.30", arch: "amd64" },
      cache1: { host: "192.168.1.40", arch: "amd64" },
    },
    services: {
      "web-frontend": {
        image: "nginx",
        hosts: ["web1"],
      },
      "web-api": {
        image: "node",
        hosts: ["web2"],
      },
      database: {
        image: "postgres",
        hosts: ["db1"],
      },
      cache: {
        image: "redis",
        hosts: ["cache1"],
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
    servers: {
      web1: { host: "192.168.1.10", arch: "amd64" },
      api1: { host: "192.168.1.20", arch: "amd64" },
    },
    services: {
      web: { image: "nginx", hosts: ["web1"] },
      api: { image: "node", hosts: ["api1"] },
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
    servers: {
      web1: { host: "192.168.1.10", arch: "amd64" },
      web2: { host: "192.168.1.20", arch: "amd64" },
      api1: { host: "192.168.1.30", arch: "amd64" },
    },
    services: {
      "web-frontend": { image: "nginx", hosts: ["web1"] },
      "web-api": { image: "node", hosts: ["web2"] },
      "api-worker": { image: "worker", hosts: ["api1"] },
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
    servers: {
      server1: { host: "192.168.1.10", arch: "amd64" },
      server2: { host: "192.168.1.20", arch: "amd64" },
      server3: { host: "192.168.1.30", arch: "amd64" },
      server4: { host: "192.168.1.40", arch: "amd64" },
    },
    services: {
      "web-frontend": { image: "nginx", hosts: ["server1"] },
      "web-api": { image: "node", hosts: ["server2"] },
      "api-worker": { image: "worker", hosts: ["server3"] },
      "api-processor": {
        image: "processor",
        hosts: ["server4"],
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
    servers: {
      web1: { host: "192.168.1.10", arch: "amd64" },
    },
    services: {
      web: { image: "nginx", hosts: ["web1"] },
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
    servers: {
      server1: { host: "192.168.1.10", arch: "amd64" },
      server2: { host: "192.168.1.20", arch: "amd64" },
    },
    services: {
      "web-api": { image: "node", hosts: ["server1"] },
      "web-frontend": { image: "nginx", hosts: ["server2"] },
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
    servers: {
      server1: { host: "192.168.1.10", arch: "amd64" },
      server2: { host: "192.168.1.20", arch: "amd64" },
      server3: { host: "192.168.1.30", arch: "amd64" },
    },
    services: {
      web1: { image: "nginx", hosts: ["server1"] },
      web2: { image: "nginx", hosts: ["server2"] },
      web12: { image: "nginx", hosts: ["server3"] },
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
    servers: {
      server1: { host: "192.168.1.10", arch: "amd64" },
      server2: { host: "192.168.1.20", arch: "amd64" },
      server3: { host: "192.168.1.30", arch: "amd64" },
    },
    services: {
      web: { image: "nginx", hosts: ["server1"] },
      Web: { image: "nginx", hosts: ["server2"] },
      WEB: { image: "nginx", hosts: ["server3"] },
    },
  });

  const services = config.getMatchingServiceNames(["web"]);
  assertEquals(services, ["web"]);
});
