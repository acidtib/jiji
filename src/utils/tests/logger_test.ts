import { assertEquals, assertStringIncludes } from "@std/assert";
import { log, Logger } from "../logger.ts";

// Mock console methods to capture output
let capturedLogs: string[] = [];

const originalConsoleLog = console.log;
const originalConsoleError = console.error;
const originalConsoleWarn = console.warn;

function mockConsole() {
  capturedLogs = [];
  console.log = (message: string) => capturedLogs.push(message);
  console.error = (message: string) => capturedLogs.push(message);
  console.warn = (message: string) => capturedLogs.push(message);
}

function restoreConsole() {
  console.log = originalConsoleLog;
  console.error = originalConsoleError;
  console.warn = originalConsoleWarn;
}

Deno.test("Logger basic functionality", () => {
  mockConsole();

  const logger = new Logger({ colors: false, showTimestamp: false });

  logger.info("test info message");
  logger.success("test success message");
  logger.warn("test warning message");
  logger.error("test error message");

  assertEquals(capturedLogs.length, 4);
  assertStringIncludes(capturedLogs[0], "[INFO ]");
  assertStringIncludes(capturedLogs[0], "test info message");
  assertStringIncludes(capturedLogs[1], "[SUCCESS]");
  assertStringIncludes(capturedLogs[1], "test success message");
  assertStringIncludes(capturedLogs[2], "[WARN ]");
  assertStringIncludes(capturedLogs[2], "test warning message");
  assertStringIncludes(capturedLogs[3], "[ERROR]");
  assertStringIncludes(capturedLogs[3], "test error message");

  restoreConsole();
});

Deno.test("Logger with prefix", () => {
  mockConsole();

  const logger = new Logger({
    prefix: "test-server",
    colors: false,
    showTimestamp: false,
    maxPrefixLength: 15,
  });

  logger.info("test message");

  assertEquals(capturedLogs.length, 1);
  assertStringIncludes(capturedLogs[0], "test-server");
  assertStringIncludes(capturedLogs[0], "[INFO ]");
  assertStringIncludes(capturedLogs[0], "test message");

  restoreConsole();
});

Deno.test("Logger executing method", () => {
  mockConsole();

  const logger = new Logger({ colors: false, showTimestamp: false });

  logger.executing("docker pull myapp:latest", "web-1");

  assertEquals(capturedLogs.length, 1);
  assertStringIncludes(capturedLogs[0], "web-1");
  assertStringIncludes(capturedLogs[0], "$ docker pull myapp:latest");

  restoreConsole();
});

Deno.test("Logger status method", () => {
  mockConsole();

  const logger = new Logger({ colors: false, showTimestamp: false });

  logger.status("Deployment in progress", "deploy");

  assertEquals(capturedLogs.length, 1);
  assertStringIncludes(capturedLogs[0], "deploy");
  assertStringIncludes(capturedLogs[0], "Deployment in progress");

  restoreConsole();
});

Deno.test("Logger child creation", () => {
  const parentLogger = new Logger({
    prefix: "parent",
    colors: false,
    showTimestamp: false,
  });

  const childLogger = parentLogger.child("child");

  mockConsole();

  childLogger.info("test message");

  assertEquals(capturedLogs.length, 1);
  assertStringIncludes(capturedLogs[0], "child");
  assertStringIncludes(capturedLogs[0], "test message");

  restoreConsole();
});

Deno.test("Logger.forServers creates multiple loggers", () => {
  const servers = ["web-1", "web-2", "db-primary"];
  const loggers = Logger.forServers(servers, {
    colors: false,
    showTimestamp: false,
  });

  assertEquals(loggers.size, 3);
  assertEquals(Array.from(loggers.keys()).sort(), servers.sort());

  mockConsole();

  loggers.get("web-1")?.info("test message from web-1");
  loggers.get("web-2")?.info("test message from web-2");

  assertEquals(capturedLogs.length, 2);
  assertStringIncludes(capturedLogs[0], "web-1");
  assertStringIncludes(capturedLogs[0], "test message from web-1");
  assertStringIncludes(capturedLogs[1], "web-2");
  assertStringIncludes(capturedLogs[1], "test message from web-2");

  restoreConsole();
});

Deno.test("Logger progress method", () => {
  mockConsole();

  const logger = new Logger({ colors: false, showTimestamp: false });

  logger.progress("Uploading files", 5, 10, "upload");

  assertEquals(capturedLogs.length, 1);
  assertStringIncludes(capturedLogs[0], "upload");
  assertStringIncludes(capturedLogs[0], "Uploading files");
  assertStringIncludes(capturedLogs[0], "50%");
  assertStringIncludes(capturedLogs[0], "(5/10)");

  restoreConsole();
});

Deno.test("Logger with timestamp", () => {
  mockConsole();

  const logger = new Logger({ colors: false, showTimestamp: true });

  logger.info("test message");

  assertEquals(capturedLogs.length, 1);
  // Check that the log contains a timestamp pattern (HH:MM:SS.mmm)
  const timestampRegex = /\d{2}:\d{2}:\d{2}\.\d{3}/;
  assertEquals(timestampRegex.test(capturedLogs[0]), true);

  restoreConsole();
});

Deno.test("Logger without timestamp", () => {
  mockConsole();

  const logger = new Logger({ colors: false, showTimestamp: false });

  logger.info("test message");

  assertEquals(capturedLogs.length, 1);
  // Check that the log does not contain a timestamp pattern
  const timestampRegex = /\d{2}:\d{2}:\d{2}\.\d{3}/;
  assertEquals(timestampRegex.test(capturedLogs[0]), false);

  restoreConsole();
});

Deno.test("Default log utility functions", () => {
  mockConsole();

  log.info("info test");
  log.success("success test");
  log.warn("warn test");
  log.error("error test");

  assertEquals(capturedLogs.length, 4);
  assertStringIncludes(capturedLogs[0], "info test");
  assertStringIncludes(capturedLogs[1], "success test");
  assertStringIncludes(capturedLogs[2], "warn test");
  assertStringIncludes(capturedLogs[3], "error test");

  restoreConsole();
});

Deno.test("Logger prefix truncation", () => {
  mockConsole();

  const logger = new Logger({
    colors: false,
    showTimestamp: false,
    maxPrefixLength: 10,
  });

  logger.info("test message", "very-long-server-name-that-exceeds-limit");

  assertEquals(capturedLogs.length, 1);
  assertStringIncludes(capturedLogs[0], "very-lo...");

  restoreConsole();
});

Deno.test("Logger group method", async () => {
  mockConsole();

  let groupExecuted = false;

  await log.group("Test Group", async () => {
    groupExecuted = true;
    await new Promise((resolve) => setTimeout(resolve, 10));
  });

  assertEquals(groupExecuted, true);
  // The group method should produce multiple log entries (separators and title)
  assertEquals(capturedLogs.length > 1, true);

  restoreConsole();
});
