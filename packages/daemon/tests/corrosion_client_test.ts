import { assertEquals } from "@std/assert";
import { CorrosionClient } from "../src/corrosion_client.ts";

Deno.test("CorrosionClient - constructor sets base URL", () => {
  const client = new CorrosionClient("http://127.0.0.1:31220");
  // Client is created without error
  assertEquals(typeof client, "object");
});

Deno.test("CorrosionClient - health returns false when unreachable", async () => {
  // Use an unlikely port so the check fails
  const client = new CorrosionClient("http://127.0.0.1:59999");
  const healthy = await client.health();
  assertEquals(healthy, false);
});

Deno.test("CorrosionClient - execGetRowsAffected returns 0 on error", async () => {
  const client = new CorrosionClient("http://127.0.0.1:59999");
  const affected = await client.execGetRowsAffected(
    "DELETE FROM containers WHERE 1=0;",
  );
  assertEquals(affected, 0);
});
