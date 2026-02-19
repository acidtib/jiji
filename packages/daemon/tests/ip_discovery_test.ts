import { assertEquals } from "@std/assert";
import { discoverPublicIp } from "../src/ip_discovery.ts";

Deno.test("discoverPublicIp - returns valid IP or null", async () => {
  const ip = await discoverPublicIp();

  if (ip !== null) {
    // Should match IPv4 format
    const ipRegex = /^\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}$/;
    assertEquals(ipRegex.test(ip), true, `Invalid IP format: ${ip}`);
  }
  // null is also acceptable (no internet connectivity)
});
