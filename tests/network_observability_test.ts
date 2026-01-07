/**
 * Network observability integration tests
 *
 * Tests the new observability functions for Phase 2:
 * - queryStaleContainers
 * - queryOfflineServers
 * - queryAllContainersWithDetails
 * - queryContainerById
 * - executeSql
 * - getDbStats
 */

import { assertEquals, assertExists } from "@std/assert";
import { MockSSHManager } from "./mocks.ts";
import {
  type ContainerWithDetails,
  type DbStats,
  deleteContainersByIds,
  deleteContainersByServer,
  executeSql,
  getDbStats,
  type OfflineServer,
  queryAllContainersWithDetails,
  queryContainerById,
  queryOfflineServers,
  queryStaleContainers,
  type StaleContainer,
} from "../src/lib/network/corrosion.ts";

// Helper to cast MockSSHManager to SSHManager type for tests
// deno-lint-ignore no-explicit-any
const asSsh = (mock: MockSSHManager): any => mock;

Deno.test("queryStaleContainers returns containers unhealthy longer than threshold", async () => {
  const mockSsh = new MockSSHManager("test-server");

  // Mock the Corrosion query response
  // Format: id|service|server_id|started_at
  const now = Math.floor(Date.now() / 1000);
  const oldTimestamp = (now - 300) * 1000; // 5 minutes ago in ms

  mockSsh.addMockResponse("corrosion query", {
    success: true,
    stdout:
      `abc123|web|srv-001|${oldTimestamp}\ndef456|api|srv-002|${oldTimestamp}`,
    stderr: "",
    code: 0,
  });

  const result: StaleContainer[] = await queryStaleContainers(
    asSsh(mockSsh),
    180,
  );

  assertEquals(result.length, 2);
  assertEquals(result[0].id, "abc123");
  assertEquals(result[0].service, "web");
  assertEquals(result[0].serverId, "srv-001");
  assertEquals(result[1].id, "def456");
  assertEquals(result[1].service, "api");
});

Deno.test("queryStaleContainers returns empty array when no stale containers", async () => {
  const mockSsh = new MockSSHManager("test-server");

  mockSsh.addMockResponse("corrosion query", {
    success: true,
    stdout: "",
    stderr: "",
    code: 0,
  });

  const result = await queryStaleContainers(asSsh(mockSsh), 180);

  assertEquals(result.length, 0);
});

Deno.test("queryOfflineServers returns servers with stale heartbeats", async () => {
  const mockSsh = new MockSSHManager("test-server");

  const oldTimestamp = Date.now() - 700000; // 11+ minutes ago

  mockSsh.addMockResponse("corrosion query", {
    success: true,
    stdout:
      `srv-001|server1.example.com|${oldTimestamp}|3\nsrv-002|server2.example.com|${oldTimestamp}|1`,
    stderr: "",
    code: 0,
  });

  const result: OfflineServer[] = await queryOfflineServers(
    asSsh(mockSsh),
    600000,
  );

  assertEquals(result.length, 2);
  assertEquals(result[0].id, "srv-001");
  assertEquals(result[0].hostname, "server1.example.com");
  assertEquals(result[0].containerCount, 3);
  assertEquals(result[1].containerCount, 1);
});

Deno.test("queryAllContainersWithDetails returns container info with server hostname", async () => {
  const mockSsh = new MockSSHManager("test-server");

  // Format: id|service|server_id|hostname|ip|healthy|started_at|instance_id
  mockSsh.addMockResponse("corrosion query", {
    success: true,
    stdout:
      "abc123|web|srv-001|server1.example.com|10.210.1.5|1|1704067200000|primary\ndef456|api|srv-002|server2.example.com|10.210.2.6|0|1704067200000|",
    stderr: "",
    code: 0,
  });

  const result: ContainerWithDetails[] = await queryAllContainersWithDetails(
    asSsh(mockSsh),
  );

  assertEquals(result.length, 2);
  assertEquals(result[0].id, "abc123");
  assertEquals(result[0].service, "web");
  assertEquals(result[0].serverHostname, "server1.example.com");
  assertEquals(result[0].ip, "10.210.1.5");
  assertEquals(result[0].healthy, true);
  assertEquals(result[0].instanceId, "primary");

  assertEquals(result[1].id, "def456");
  assertEquals(result[1].healthy, false);
  assertEquals(result[1].instanceId, undefined);
});

Deno.test("queryAllContainersWithDetails filters by service", async () => {
  const mockSsh = new MockSSHManager("test-server");

  mockSsh.addMockResponse("corrosion query", {
    success: true,
    stdout:
      "abc123|web|srv-001|server1.example.com|10.210.1.5|1|1704067200000|",
    stderr: "",
    code: 0,
  });

  const result = await queryAllContainersWithDetails(asSsh(mockSsh), "web");

  assertEquals(result.length, 1);
  assertEquals(result[0].service, "web");

  // Verify the query included the WHERE clause
  const commands = mockSsh.getAllCommands();
  const queryCmd = commands.find((c) => c.includes("corrosion query"));
  assertExists(queryCmd);
  assertEquals(queryCmd.includes("WHERE c.service = 'web'"), true);
});

Deno.test("queryContainerById finds container by partial ID", async () => {
  const mockSsh = new MockSSHManager("test-server");

  mockSsh.addMockResponse("corrosion query", {
    success: true,
    stdout:
      "abc123def456|web|srv-001|server1.example.com|10.210.1.5|1|1704067200000|primary",
    stderr: "",
    code: 0,
  });

  const result = await queryContainerById(asSsh(mockSsh), "abc123");

  assertExists(result);
  assertEquals(result.id, "abc123def456");
  assertEquals(result.service, "web");
  assertEquals(result.instanceId, "primary");
});

Deno.test("queryContainerById returns null when not found", async () => {
  const mockSsh = new MockSSHManager("test-server");

  mockSsh.addMockResponse("corrosion query", {
    success: true,
    stdout: "",
    stderr: "",
    code: 0,
  });

  const result = await queryContainerById(asSsh(mockSsh), "nonexistent");

  assertEquals(result, null);
});

Deno.test("deleteContainersByIds executes delete with correct IDs", async () => {
  const mockSsh = new MockSSHManager("test-server");

  mockSsh.addMockResponse("corrosion exec", {
    success: true,
    stdout: "",
    stderr: "",
    code: 0,
  });

  const count = await deleteContainersByIds(asSsh(mockSsh), [
    "abc123",
    "def456",
  ]);

  assertEquals(count, 2);

  const commands = mockSsh.getAllCommands();
  const execCmd = commands.find((c) => c.includes("corrosion exec"));
  assertExists(execCmd);
  assertEquals(execCmd.includes("DELETE FROM containers WHERE id IN"), true);
  assertEquals(execCmd.includes("'abc123'"), true);
  assertEquals(execCmd.includes("'def456'"), true);
});

Deno.test("deleteContainersByIds returns 0 for empty array", async () => {
  const mockSsh = new MockSSHManager("test-server");

  const count = await deleteContainersByIds(asSsh(mockSsh), []);

  assertEquals(count, 0);
  assertEquals(mockSsh.getAllCommands().length, 0);
});

Deno.test("deleteContainersByServer counts and deletes containers", async () => {
  const mockSsh = new MockSSHManager("test-server");

  // First call is COUNT query
  mockSsh.addMockResponse("SELECT COUNT", {
    success: true,
    stdout: "5",
    stderr: "",
    code: 0,
  });

  // Second call is DELETE
  mockSsh.addMockResponse("DELETE FROM containers WHERE server_id", {
    success: true,
    stdout: "",
    stderr: "",
    code: 0,
  });

  const count = await deleteContainersByServer(asSsh(mockSsh), "srv-001");

  assertEquals(count, 5);
});

Deno.test("executeSql passes query to Corrosion", async () => {
  const mockSsh = new MockSSHManager("test-server");

  mockSsh.addMockResponse("corrosion query", {
    success: true,
    stdout: "result1|result2\nresult3|result4",
    stderr: "",
    code: 0,
  });

  const result = await executeSql(asSsh(mockSsh), "SELECT * FROM containers;");

  assertEquals(result.includes("result1"), true);
  assertEquals(result.includes("result3"), true);
});

Deno.test("getDbStats returns database statistics", async () => {
  const mockSsh = new MockSSHManager("test-server");

  // Format: server_count|active_server_count|container_count|healthy|unhealthy|service_count
  mockSsh.addMockResponse("corrosion query", {
    success: true,
    stdout: "3|2|10|8|2|4",
    stderr: "",
    code: 0,
  });

  const stats: DbStats = await getDbStats(asSsh(mockSsh));

  assertEquals(stats.serverCount, 3);
  assertEquals(stats.activeServerCount, 2);
  assertEquals(stats.containerCount, 10);
  assertEquals(stats.healthyContainerCount, 8);
  assertEquals(stats.unhealthyContainerCount, 2);
  assertEquals(stats.serviceCount, 4);
});

Deno.test("getDbStats handles zero values", async () => {
  const mockSsh = new MockSSHManager("test-server");

  mockSsh.addMockResponse("corrosion query", {
    success: true,
    stdout: "0|0|0|0|0|0",
    stderr: "",
    code: 0,
  });

  const stats = await getDbStats(asSsh(mockSsh));

  assertEquals(stats.serverCount, 0);
  assertEquals(stats.containerCount, 0);
});
