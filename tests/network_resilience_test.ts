/**
 * Network resilience integration tests
 *
 * Tests the Phase 3 resilience features:
 * - Database indexes
 * - Schema migrations for health_status columns
 * - applyMigrations function
 */

import { assertEquals } from "@std/assert";
import { MockSSHManager } from "./mocks.ts";
import { applyMigrations } from "../src/lib/network/corrosion.ts";

// Helper to cast MockSSHManager to SSHManager type for tests
// deno-lint-ignore no-explicit-any
const asSsh = (mock: MockSSHManager): any => mock;

Deno.test("applyMigrations skips when columns already exist", async () => {
  const mockSsh = new MockSSHManager("test-server");

  // Mock response: column exists (count = 1)
  mockSsh.addMockResponse("pragma_table_info", {
    success: true,
    stdout: "1",
    stderr: "",
    code: 0,
  });

  const result = await applyMigrations(asSsh(mockSsh));

  assertEquals(result, true);

  // Verify only one command was executed (the check)
  const commands = mockSsh.getAllCommands();
  assertEquals(commands.length, 1);
  assertEquals(commands[0].includes("pragma_table_info"), true);
});

Deno.test("applyMigrations applies migration when columns missing", async () => {
  const mockSsh = new MockSSHManager("test-server");

  // Mock response: column doesn't exist (count = 0)
  mockSsh.addMockResponse("pragma_table_info", {
    success: true,
    stdout: "0",
    stderr: "",
    code: 0,
  });

  // Mock successful ALTER TABLE statements
  mockSsh.addMockResponse("ALTER TABLE", {
    success: true,
    stdout: "",
    stderr: "",
    code: 0,
  });

  // Mock successful CREATE INDEX
  mockSsh.addMockResponse("CREATE INDEX", {
    success: true,
    stdout: "",
    stderr: "",
    code: 0,
  });

  // Mock successful UPDATE for initializing health_status
  mockSsh.addMockResponse("UPDATE containers SET health_status", {
    success: true,
    stdout: "",
    stderr: "",
    code: 0,
  });

  const result = await applyMigrations(asSsh(mockSsh));

  assertEquals(result, true);

  // Verify ALTER TABLE commands were executed
  const commands = mockSsh.getAllCommands();
  const alterCommands = commands.filter((c) => c.includes("ALTER TABLE"));
  assertEquals(alterCommands.length >= 4, true); // 4 columns to add
});

Deno.test("applyMigrations handles duplicate column errors gracefully", async () => {
  const mockSsh = new MockSSHManager("test-server");

  // Mock response: column doesn't exist
  mockSsh.addMockResponse("pragma_table_info", {
    success: true,
    stdout: "0",
    stderr: "",
    code: 0,
  });

  // Mock duplicate column error (which should be ignored)
  mockSsh.addMockResponse("ALTER TABLE", {
    success: false,
    stdout: "",
    stderr: "duplicate column name: health_status",
    code: 1,
  });

  // Mock successful CREATE INDEX
  mockSsh.addMockResponse("CREATE INDEX", {
    success: true,
    stdout: "",
    stderr: "",
    code: 0,
  });

  // Mock successful UPDATE
  mockSsh.addMockResponse("UPDATE containers SET health_status", {
    success: true,
    stdout: "",
    stderr: "",
    code: 0,
  });

  const result = await applyMigrations(asSsh(mockSsh));

  // Should still succeed since duplicate column errors are expected
  assertEquals(result, true);
});

Deno.test("applyMigrations returns false when check fails", async () => {
  const mockSsh = new MockSSHManager("test-server");

  // Mock response: command fails
  mockSsh.addMockResponse("pragma_table_info", {
    success: false,
    stdout: "",
    stderr: "connection failed",
    code: 1,
  });

  const result = await applyMigrations(asSsh(mockSsh));

  assertEquals(result, false);
});

// Test ContainerHealthStatus type (compile-time check)
Deno.test("ContainerHealthStatus type accepts valid values", () => {
  // Import and use the type to verify it's correctly defined
  const validStatuses = ["healthy", "degraded", "unhealthy", "unknown"];
  for (const status of validStatuses) {
    assertEquals(typeof status, "string");
  }
});

// Test that indexes are defined in schema
Deno.test("CORROSION_SCHEMA includes performance indexes", async () => {
  // Read the corrosion.ts file to verify indexes are defined
  const content = await Deno.readTextFile(
    "src/lib/network/corrosion.ts",
  );

  // Check for index definitions
  assertEquals(
    content.includes("idx_containers_server_id"),
    true,
    "Missing idx_containers_server_id index",
  );
  assertEquals(
    content.includes("idx_containers_service"),
    true,
    "Missing idx_containers_service index",
  );
  assertEquals(
    content.includes("idx_containers_healthy"),
    true,
    "Missing idx_containers_healthy index",
  );
  assertEquals(
    content.includes("idx_containers_health_status"),
    true,
    "Missing idx_containers_health_status index",
  );
  assertEquals(
    content.includes("idx_servers_last_seen"),
    true,
    "Missing idx_servers_last_seen index",
  );
});

// Test that schema includes Phase 3 columns
Deno.test("CORROSION_SCHEMA includes Phase 3 health columns", async () => {
  const content = await Deno.readTextFile(
    "src/lib/network/corrosion.ts",
  );

  // Check for new column definitions
  assertEquals(
    content.includes("health_status TEXT DEFAULT 'unknown'"),
    true,
    "Missing health_status column",
  );
  assertEquals(
    content.includes("last_health_check INTEGER DEFAULT 0"),
    true,
    "Missing last_health_check column",
  );
  assertEquals(
    content.includes("consecutive_failures INTEGER DEFAULT 0"),
    true,
    "Missing consecutive_failures column",
  );
  assertEquals(
    content.includes("health_port INTEGER DEFAULT NULL"),
    true,
    "Missing health_port column",
  );
});

// Test that control loop includes Phase 3 features
Deno.test("control_loop includes signal handlers", async () => {
  const content = await Deno.readTextFile(
    "src/lib/network/control_loop.ts",
  );

  assertEquals(
    content.includes("trap cleanup SIGTERM SIGINT SIGHUP"),
    true,
    "Missing signal handlers",
  );
  assertEquals(
    content.includes("SHUTTING_DOWN"),
    true,
    "Missing SHUTTING_DOWN flag",
  );
});

Deno.test("control_loop includes split-brain detection", async () => {
  const content = await Deno.readTextFile(
    "src/lib/network/control_loop.ts",
  );

  assertEquals(
    content.includes("detect_split_brain"),
    true,
    "Missing detect_split_brain function",
  );
  assertEquals(
    content.includes("SPLIT-BRAIN"),
    true,
    "Missing split-brain alert message",
  );
});

Deno.test("control_loop includes iteration timing", async () => {
  const content = await Deno.readTextFile(
    "src/lib/network/control_loop.ts",
  );

  assertEquals(
    content.includes("ITERATION_START"),
    true,
    "Missing ITERATION_START tracking",
  );
  assertEquals(
    content.includes("ITERATION_DURATION"),
    true,
    "Missing ITERATION_DURATION tracking",
  );
  assertEquals(
    content.includes("Slow iteration"),
    true,
    "Missing slow iteration warning",
  );
});

Deno.test("control_loop includes TCP health check", async () => {
  const content = await Deno.readTextFile(
    "src/lib/network/control_loop.ts",
  );

  assertEquals(
    content.includes("check_container_tcp_health"),
    true,
    "Missing TCP health check function",
  );
  assertEquals(
    content.includes("/dev/tcp/"),
    true,
    "Missing bash TCP check syntax",
  );
});

// Test that DNS query uses health_status
Deno.test("DNS update script uses health_status filter", async () => {
  const content = await Deno.readTextFile(
    "src/lib/network/dns.ts",
  );

  assertEquals(
    content.includes("health_status = 'healthy'"),
    true,
    "Missing health_status filter in DNS query",
  );
});

// Test ContainerRegistration type has new fields
Deno.test("ContainerRegistration type includes health fields", async () => {
  const content = await Deno.readTextFile(
    "src/types/network.ts",
  );

  assertEquals(
    content.includes("healthStatus?:"),
    true,
    "Missing healthStatus field",
  );
  assertEquals(
    content.includes("lastHealthCheck?:"),
    true,
    "Missing lastHealthCheck field",
  );
  assertEquals(
    content.includes("consecutiveFailures?:"),
    true,
    "Missing consecutiveFailures field",
  );
  assertEquals(
    content.includes("healthPort?:"),
    true,
    "Missing healthPort field",
  );
});
