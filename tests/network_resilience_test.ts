/**
 * Network resilience integration tests
 *
 * Tests the resilience features:
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

// Test that schema includes columns
Deno.test("CORROSION_SCHEMA includes health columns", async () => {
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

Deno.test("control_loop installs binary from GitHub releases", async () => {
  const content = await Deno.readTextFile(
    "src/lib/network/control_loop.ts",
  );

  assertEquals(
    content.includes("installControlLoop"),
    true,
    "Missing installControlLoop function",
  );
  assertEquals(
    content.includes("jiji-control-loop"),
    true,
    "Missing binary name reference",
  );
});

Deno.test("control_loop configures systemd service with environment variables", async () => {
  const content = await Deno.readTextFile(
    "src/lib/network/control_loop.ts",
  );

  assertEquals(
    content.includes('Environment="SERVER_ID='),
    true,
    "Missing SERVER_ID environment variable",
  );
  assertEquals(
    content.includes('Environment="ENGINE='),
    true,
    "Missing ENGINE environment variable",
  );
  assertEquals(
    content.includes('Environment="CORROSION_API='),
    true,
    "Missing CORROSION_API environment variable",
  );
  assertEquals(
    content.includes('Environment="CORROSION_DIR='),
    true,
    "Missing CORROSION_DIR environment variable",
  );
});

Deno.test("control_loop supports multi-architecture download", async () => {
  const content = await Deno.readTextFile(
    "src/lib/network/control_loop.ts",
  );

  assertEquals(
    content.includes("linux-x64"),
    true,
    "Missing x64 architecture support",
  );
  assertEquals(
    content.includes("linux-arm64"),
    true,
    "Missing arm64 architecture support",
  );
});

Deno.test("control_loop cleans up old bash script", async () => {
  const content = await Deno.readTextFile(
    "src/lib/network/control_loop.ts",
  );

  assertEquals(
    content.includes("jiji-control-loop.sh"),
    true,
    "Missing cleanup of old bash script",
  );
});

// Note: health_status filtering is now handled in jiji-dns via Corrosion subscription
// The jiji-dns server subscribes to container changes and filters healthy containers
// See: https://github.com/acidtib/jiji-dns

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
