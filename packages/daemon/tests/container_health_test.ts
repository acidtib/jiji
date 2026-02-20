import { assertEquals } from "@std/assert";
import { checkTcpHealth } from "../src/container_health.ts";

Deno.test("checkTcpHealth - returns false for unreachable host", async () => {
  // Connect to a port that is almost certainly not listening
  const result = await checkTcpHealth("127.0.0.1", 59998, 500);
  assertEquals(result, false);
});

Deno.test("checkTcpHealth - times out after specified duration", async () => {
  const start = Date.now();
  await checkTcpHealth("192.0.2.1", 80, 500); // Non-routable IP
  const elapsed = Date.now() - start;

  // Should complete within ~500-1000ms (timeout + overhead)
  assertEquals(elapsed < 2000, true, `Elapsed: ${elapsed}ms`);
});
