import { assertEquals } from "@std/assert";
import { setServerId } from "../src/logger.ts";
import * as log from "../src/logger.ts";

Deno.test("logger - outputs valid JSON", () => {
  setServerId("test-server");

  // Capture console.log output
  const originalLog = console.log;
  let captured = "";
  console.log = (msg: string) => {
    captured = msg;
  };

  try {
    log.info("test message");
    const parsed = JSON.parse(captured);

    assertEquals(parsed.level, "info");
    assertEquals(parsed.server_id, "test-server");
    assertEquals(parsed.message, "test message");
    assertEquals(typeof parsed.timestamp, "string");
  } finally {
    console.log = originalLog;
  }
});

Deno.test("logger - includes data when provided", () => {
  setServerId("test-server");

  const originalLog = console.log;
  let captured = "";
  console.log = (msg: string) => {
    captured = msg;
  };

  try {
    log.info("health check", { container_id: "abc123", status: "healthy" });
    const parsed = JSON.parse(captured);

    assertEquals(parsed.data.container_id, "abc123");
    assertEquals(parsed.data.status, "healthy");
  } finally {
    console.log = originalLog;
  }
});

Deno.test("logger - omits data when not provided", () => {
  setServerId("test-server");

  const originalLog = console.log;
  let captured = "";
  console.log = (msg: string) => {
    captured = msg;
  };

  try {
    log.warn("simple warning");
    const parsed = JSON.parse(captured);

    assertEquals(parsed.level, "warn");
    assertEquals(parsed.data, undefined);
  } finally {
    console.log = originalLog;
  }
});

Deno.test("logger - all log levels work", () => {
  setServerId("test-server");

  const originalLog = console.log;
  const levels: string[] = [];
  console.log = (msg: string) => {
    levels.push(JSON.parse(msg).level);
  };

  try {
    log.info("info");
    log.warn("warn");
    log.error("error");
    log.debug("debug");

    assertEquals(levels, ["info", "warn", "error", "debug"]);
  } finally {
    console.log = originalLog;
  }
});

Deno.test("logger - timestamp is ISO format", () => {
  setServerId("test-server");

  const originalLog = console.log;
  let captured = "";
  console.log = (msg: string) => {
    captured = msg;
  };

  try {
    log.info("test");
    const parsed = JSON.parse(captured);
    const date = new Date(parsed.timestamp);

    // Should be a valid date
    assertEquals(isNaN(date.getTime()), false);
  } finally {
    console.log = originalLog;
  }
});
