import { assertEquals } from "@std/assert";
import { parseEndpoints } from "../src/peer_reconciler.ts";

Deno.test("parseEndpoints - valid JSON array", () => {
  const result = parseEndpoints('["1.2.3.4:31820","5.6.7.8:31820"]');
  assertEquals(result, ["1.2.3.4:31820", "5.6.7.8:31820"]);
});

Deno.test("parseEndpoints - single endpoint", () => {
  const result = parseEndpoints('["10.0.0.1:31820"]');
  assertEquals(result, ["10.0.0.1:31820"]);
});

Deno.test("parseEndpoints - empty array", () => {
  const result = parseEndpoints("[]");
  assertEquals(result, []);
});

Deno.test("parseEndpoints - malformed JSON returns empty", () => {
  const result = parseEndpoints("not json");
  assertEquals(result, []);
});

Deno.test("parseEndpoints - empty string returns empty", () => {
  const result = parseEndpoints("");
  assertEquals(result, []);
});

Deno.test("parseEndpoints - filters non-string values", () => {
  const result = parseEndpoints('[123, "1.2.3.4:31820", null, true]');
  assertEquals(result, ["1.2.3.4:31820"]);
});
