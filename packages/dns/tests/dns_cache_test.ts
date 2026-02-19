/**
 * Tests for DNS cache
 */

import { assertEquals } from "@std/assert";
import { DnsCache } from "../src/dns_cache.ts";
import type { DnsRecord } from "../src/types.ts";

function createRecord(overrides: Partial<DnsRecord> = {}): DnsRecord {
  return {
    containerId: "container-123",
    service: "api",
    project: "casa",
    serverId: "server1",
    ip: "10.210.1.5",
    healthy: true,
    startedAt: Date.now(),
    ...overrides,
  };
}

Deno.test("DnsCache - set and get single record", () => {
  const cache = new DnsCache();
  const record = createRecord();

  cache.set(record);

  const ips = cache.get("casa-api");
  assertEquals(ips, ["10.210.1.5"]);
});

Deno.test("DnsCache - get returns empty for unknown hostname", () => {
  const cache = new DnsCache();

  const ips = cache.get("unknown-service");
  assertEquals(ips, []);
});

Deno.test("DnsCache - get is case insensitive", () => {
  const cache = new DnsCache();
  const record = createRecord();

  cache.set(record);

  assertEquals(cache.get("casa-api"), ["10.210.1.5"]);
  assertEquals(cache.get("CASA-API"), ["10.210.1.5"]);
  assertEquals(cache.get("Casa-Api"), ["10.210.1.5"]);
});

Deno.test("DnsCache - multiple containers for same service", () => {
  const cache = new DnsCache();

  // Add two containers on different servers
  cache.set(createRecord({
    containerId: "container-1",
    serverId: "server1",
    ip: "10.210.1.5",
    startedAt: 1000,
  }));

  cache.set(createRecord({
    containerId: "container-2",
    serverId: "server2",
    ip: "10.210.2.3",
    startedAt: 2000,
  }));

  const ips = cache.get("casa-api");
  assertEquals(ips.length, 2);
  assertEquals(ips.includes("10.210.1.5"), true);
  assertEquals(ips.includes("10.210.2.3"), true);
});

Deno.test("DnsCache - newest container wins per server", () => {
  const cache = new DnsCache();

  // Add old container
  cache.set(createRecord({
    containerId: "old-container",
    serverId: "server1",
    ip: "10.210.1.5",
    startedAt: 1000,
  }));

  // Add newer container on same server
  cache.set(createRecord({
    containerId: "new-container",
    serverId: "server1",
    ip: "10.210.1.6",
    startedAt: 2000,
  }));

  // Should only return the newer container's IP
  const ips = cache.get("casa-api");
  assertEquals(ips, ["10.210.1.6"]);
});

Deno.test("DnsCache - unhealthy containers not returned", () => {
  const cache = new DnsCache();

  cache.set(createRecord({
    containerId: "healthy-container",
    ip: "10.210.1.5",
    healthy: true,
  }));

  cache.set(createRecord({
    containerId: "unhealthy-container",
    serverId: "server2",
    ip: "10.210.2.3",
    healthy: false,
  }));

  const ips = cache.get("casa-api");
  assertEquals(ips, ["10.210.1.5"]);
});

Deno.test("DnsCache - instance-specific hostname", () => {
  const cache = new DnsCache();

  cache.set(createRecord({
    instanceId: "server1",
  }));

  // Should be accessible via both hostnames
  assertEquals(cache.get("casa-api"), ["10.210.1.5"]);
  assertEquals(cache.get("casa-api-server1"), ["10.210.1.5"]);
});

Deno.test("DnsCache - remove by container ID", () => {
  const cache = new DnsCache();
  const record = createRecord();

  cache.set(record);
  assertEquals(cache.get("casa-api"), ["10.210.1.5"]);

  const removed = cache.remove("container-123");
  assertEquals(removed, true);
  assertEquals(cache.get("casa-api"), []);
});

Deno.test("DnsCache - remove returns false for unknown ID", () => {
  const cache = new DnsCache();

  const removed = cache.remove("unknown-id");
  assertEquals(removed, false);
});

Deno.test("DnsCache - getByContainerId", () => {
  const cache = new DnsCache();
  const record = createRecord();

  cache.set(record);

  const found = cache.getByContainerId("container-123");
  assertEquals(found?.ip, "10.210.1.5");
  assertEquals(found?.service, "api");
});

Deno.test("DnsCache - updateHealth", () => {
  const cache = new DnsCache();
  const record = createRecord({ healthy: true });

  cache.set(record);
  assertEquals(cache.get("casa-api"), ["10.210.1.5"]);

  // Mark as unhealthy
  cache.updateHealth("container-123", false);
  assertEquals(cache.get("casa-api"), []);

  // Mark as healthy again
  cache.updateHealth("container-123", true);
  assertEquals(cache.get("casa-api"), ["10.210.1.5"]);
});

Deno.test("DnsCache - clear removes all records", () => {
  const cache = new DnsCache();

  cache.set(createRecord({ containerId: "c1" }));
  cache.set(createRecord({ containerId: "c2", service: "web" }));

  assertEquals(cache.size, 2);

  cache.clear();

  assertEquals(cache.size, 0);
  assertEquals(cache.get("casa-api"), []);
  assertEquals(cache.get("casa-web"), []);
});

Deno.test("DnsCache - getStats", () => {
  const cache = new DnsCache();

  cache.set(createRecord({ containerId: "c1", healthy: true }));
  cache.set(createRecord({ containerId: "c2", service: "web", healthy: true }));
  cache.set(createRecord({ containerId: "c3", service: "db", healthy: false }));

  const stats = cache.getStats();

  assertEquals(stats.totalRecords, 3);
  assertEquals(stats.healthyRecords, 2);
  assertEquals(stats.hostnames, 3); // casa-api, casa-web, casa-db
});

Deno.test("DnsCache - hostnames property", () => {
  const cache = new DnsCache();

  cache.set(createRecord({ service: "api" }));
  cache.set(createRecord({ containerId: "c2", service: "web" }));

  const hostnames = cache.hostnames;

  assertEquals(hostnames.length, 2);
  assertEquals(hostnames.includes("casa-api"), true);
  assertEquals(hostnames.includes("casa-web"), true);
});
