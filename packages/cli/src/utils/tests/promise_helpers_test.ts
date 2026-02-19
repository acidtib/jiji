import { assertEquals } from "@std/assert";
import {
  combineAggregatedResults,
  createErrorSummary,
  executeHostOperations,
  executeWithErrorCollection,
  executeWithRetryAndErrorCollection,
  logAggregatedResults,
} from "../promise_helpers.ts";

Deno.test("executeWithErrorCollection - all operations succeed", async () => {
  const operations = [
    () => Promise.resolve("result1"),
    () => Promise.resolve("result2"),
    () => Promise.resolve("result3"),
  ];

  const result = await executeWithErrorCollection(operations);

  assertEquals(result.results, ["result1", "result2", "result3"]);
  assertEquals(result.errors, []);
  assertEquals(result.successCount, 3);
  assertEquals(result.errorCount, 0);
  assertEquals(result.totalCount, 3);
});

Deno.test("executeWithErrorCollection - mixed success and failures", async () => {
  const operations = [
    () => Promise.resolve("success1"),
    () => Promise.reject(new Error("error1")),
    () => Promise.resolve("success2"),
    () => Promise.reject(new Error("error2")),
  ];

  const result = await executeWithErrorCollection(operations);

  assertEquals(result.results, ["success1", "success2"]);
  assertEquals(result.errors.length, 2);
  assertEquals(result.errors[0].message, "error1");
  assertEquals(result.errors[1].message, "error2");
  assertEquals(result.successCount, 2);
  assertEquals(result.errorCount, 2);
  assertEquals(result.totalCount, 4);
});

Deno.test("executeWithErrorCollection - all operations fail", async () => {
  const operations = [
    () => Promise.reject(new Error("error1")),
    () => Promise.reject(new Error("error2")),
  ];

  const result = await executeWithErrorCollection(operations);

  assertEquals(result.results, []);
  assertEquals(result.errors.length, 2);
  assertEquals(result.successCount, 0);
  assertEquals(result.errorCount, 2);
  assertEquals(result.totalCount, 2);
});

Deno.test("executeHostOperations - successful operations", async () => {
  const hostOperations = [
    { host: "host1", operation: () => Promise.resolve("result1") },
    { host: "host2", operation: () => Promise.resolve("result2") },
  ];

  const result = await executeHostOperations(hostOperations);

  assertEquals(result.results, ["result1", "result2"]);
  assertEquals(result.errors, []);
  assertEquals(result.hostErrors, []);
  assertEquals(result.successCount, 2);
  assertEquals(result.errorCount, 0);
});

Deno.test("executeHostOperations - with host-specific errors", async () => {
  const hostOperations = [
    { host: "host1", operation: () => Promise.resolve("success") },
    {
      host: "host2",
      operation: () => Promise.reject(new Error("host2 error")),
    },
    {
      host: "host3",
      operation: () => Promise.reject(new Error("host3 error")),
    },
  ];

  const result = await executeHostOperations(hostOperations);

  assertEquals(result.results, ["success"]);
  assertEquals(result.errors.length, 2);
  assertEquals(result.hostErrors.length, 2);
  assertEquals(result.hostErrors[0].host, "host2");
  assertEquals(result.hostErrors[0].error.message, "host2 error");
  assertEquals(result.hostErrors[1].host, "host3");
  assertEquals(result.hostErrors[1].error.message, "host3 error");
});

Deno.test("executeWithRetryAndErrorCollection - retries failed operations", async () => {
  let attempts = 0;
  const operations = [
    () => Promise.resolve("success"),
    () => {
      attempts++;
      if (attempts < 3) {
        return Promise.reject(new Error("temporary failure"));
      }
      return Promise.resolve("success after retry");
    },
    () => Promise.reject(new Error("permanent failure")),
  ];

  const result = await executeWithRetryAndErrorCollection(operations, 3, 10);

  assertEquals(result.results, ["success", "success after retry"]);
  assertEquals(result.errors.length, 1);
  assertEquals(result.errors[0].message, "permanent failure");
  assertEquals(result.successCount, 2);
  assertEquals(result.errorCount, 1);
});

Deno.test("combineAggregatedResults - combines multiple result sets", () => {
  const resultSet1 = {
    results: ["a", "b"],
    errors: [new Error("error1")],
    successCount: 2,
    errorCount: 1,
    totalCount: 3,
  };

  const resultSet2 = {
    results: ["c"],
    errors: [new Error("error2"), new Error("error3")],
    successCount: 1,
    errorCount: 2,
    totalCount: 3,
  };

  const combined = combineAggregatedResults([resultSet1, resultSet2]);

  assertEquals(combined.results, ["a", "b", "c"]);
  assertEquals(combined.errors.length, 3);
  assertEquals(combined.successCount, 3);
  assertEquals(combined.errorCount, 3);
  assertEquals(combined.totalCount, 6);
});

Deno.test("createErrorSummary - generates appropriate summaries", () => {
  const allSuccessResults = {
    results: ["a", "b"],
    errors: [],
    successCount: 2,
    errorCount: 0,
    totalCount: 2,
  };

  const mixedResults = {
    results: ["a"],
    errors: [new Error("error")],
    successCount: 1,
    errorCount: 1,
    totalCount: 2,
  };

  const allFailureResults = {
    results: [],
    errors: [new Error("error1"), new Error("error2")],
    successCount: 0,
    errorCount: 2,
    totalCount: 2,
  };

  assertEquals(
    createErrorSummary(allSuccessResults, "Test operation"),
    "Test operation completed successfully on all 2 target(s)",
  );

  assertEquals(
    createErrorSummary(mixedResults, "Test operation"),
    "Test operation completed with mixed results: 1 succeeded, 1 failed (total: 2)",
  );

  assertEquals(
    createErrorSummary(allFailureResults, "Test operation"),
    "Test operation failed on all 2 target(s)",
  );
});

Deno.test("logAggregatedResults - logs appropriate messages", () => {
  const messages: { type: string; message: string }[] = [];
  const mockLogger = {
    success: (msg: string) => messages.push({ type: "success", message: msg }),
    warn: (msg: string) => messages.push({ type: "warn", message: msg }),
    error: (msg: string) => messages.push({ type: "error", message: msg }),
  };

  const mixedResults = {
    results: ["a"],
    errors: [new Error("test error")],
    successCount: 1,
    errorCount: 1,
    totalCount: 2,
  };

  logAggregatedResults(mixedResults, "Test operation", mockLogger);

  assertEquals(messages.length, 3);
  assertEquals(messages[0].type, "warn");
  assertEquals(messages[1].type, "error");
  assertEquals(
    messages[1].message,
    "Errors encountered during Test operation:",
  );
  assertEquals(messages[2].type, "error");
  assertEquals(messages[2].message, "  1. test error");
});

Deno.test("executeWithErrorCollection - handles non-Error rejections", async () => {
  const operations = [
    () => Promise.reject("string error"),
    () => Promise.reject(42),
    () => Promise.reject(null),
  ];

  const result = await executeWithErrorCollection(operations);

  assertEquals(result.results, []);
  assertEquals(result.errors.length, 3);
  assertEquals(result.errors[0].message, "string error");
  assertEquals(result.errors[1].message, "42");
  assertEquals(result.errors[2].message, "null");
});

Deno.test("executeHostOperations - empty operations array", async () => {
  const result = await executeHostOperations([]);

  assertEquals(result.results, []);
  assertEquals(result.errors, []);
  assertEquals(result.hostErrors, []);
  assertEquals(result.successCount, 0);
  assertEquals(result.errorCount, 0);
  assertEquals(result.totalCount, 0);
});
