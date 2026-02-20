import { assertEquals } from "@std/assert";
import {
  escapeSql,
  isValidCIDR,
  isValidContainerId,
  isValidEndpoint,
  isValidIPv4,
  isValidIPv6,
  isValidServerId,
  isValidWireGuardKey,
} from "../src/validation.ts";

// escapeSql

Deno.test("escapeSql - escapes single quotes", () => {
  assertEquals(escapeSql("it's a test"), "it''s a test");
});

Deno.test("escapeSql - no-op on clean strings", () => {
  assertEquals(escapeSql("hello"), "hello");
});

Deno.test("escapeSql - handles multiple single quotes", () => {
  assertEquals(escapeSql("a'b'c"), "a''b''c");
});

Deno.test("escapeSql - handles SQL injection attempt", () => {
  assertEquals(
    escapeSql("'; DROP TABLE servers; --"),
    "''; DROP TABLE servers; --",
  );
});

Deno.test("escapeSql - doubles each single quote", () => {
  assertEquals(escapeSql("'"), "''");
  assertEquals(escapeSql("''"), "''''");
});

// isValidContainerId

Deno.test("isValidContainerId - valid 12-char hex", () => {
  assertEquals(isValidContainerId("abcdef012345"), true);
});

Deno.test("isValidContainerId - valid 64-char hex", () => {
  assertEquals(isValidContainerId("a".repeat(64)), true);
});

Deno.test("isValidContainerId - rejects non-hex", () => {
  assertEquals(isValidContainerId("abcdefg12345"), false);
});

Deno.test("isValidContainerId - rejects too short", () => {
  assertEquals(isValidContainerId("abc"), false);
});

Deno.test("isValidContainerId - rejects injection attempt", () => {
  assertEquals(isValidContainerId("abc'; DROP TABLE--"), false);
});

// isValidServerId

Deno.test("isValidServerId - valid alphanumeric with hyphens", () => {
  assertEquals(isValidServerId("server-1"), true);
});

Deno.test("isValidServerId - valid with dots and underscores", () => {
  assertEquals(isValidServerId("my_server.prod"), true);
});

Deno.test("isValidServerId - rejects empty string", () => {
  assertEquals(isValidServerId(""), false);
});

Deno.test("isValidServerId - rejects special characters", () => {
  assertEquals(isValidServerId("server'; DROP"), false);
});

// isValidWireGuardKey

Deno.test("isValidWireGuardKey - valid base64 key", () => {
  assertEquals(
    isValidWireGuardKey("YWJjZGVmZ2hpamtsbW5vcHFyc3R1dnd4eXoxMjM0NTY="),
    true,
  );
});

Deno.test("isValidWireGuardKey - rejects wrong length", () => {
  assertEquals(isValidWireGuardKey("abc="), false);
});

Deno.test("isValidWireGuardKey - rejects injection attempt", () => {
  assertEquals(isValidWireGuardKey("'; DROP TABLE servers; --"), false);
});

// isValidIPv4

Deno.test("isValidIPv4 - valid address", () => {
  assertEquals(isValidIPv4("192.168.1.1"), true);
});

Deno.test("isValidIPv4 - rejects out of range", () => {
  assertEquals(isValidIPv4("256.1.1.1"), false);
});

Deno.test("isValidIPv4 - rejects non-IP", () => {
  assertEquals(isValidIPv4("not-an-ip"), false);
});

Deno.test("isValidIPv4 - rejects leading zeros", () => {
  assertEquals(isValidIPv4("01.02.03.04"), false);
});

// isValidIPv6

Deno.test("isValidIPv6 - valid full address", () => {
  assertEquals(isValidIPv6("2001:0db8:85a3:0000:0000:8a2e:0370:7334"), true);
});

Deno.test("isValidIPv6 - valid compressed", () => {
  assertEquals(isValidIPv6("::1"), true);
});

Deno.test("isValidIPv6 - rejects non-IP", () => {
  assertEquals(isValidIPv6("not-an-ip"), false);
});

// isValidCIDR

Deno.test("isValidCIDR - valid IPv4 CIDR", () => {
  assertEquals(isValidCIDR("10.210.1.0/24"), true);
});

Deno.test("isValidCIDR - valid IPv6 CIDR", () => {
  assertEquals(isValidCIDR("::1/128"), true);
});

Deno.test("isValidCIDR - rejects invalid prefix", () => {
  assertEquals(isValidCIDR("10.0.0.0/33"), false);
});

Deno.test("isValidCIDR - rejects no prefix", () => {
  assertEquals(isValidCIDR("10.0.0.0"), false);
});

// isValidEndpoint

Deno.test("isValidEndpoint - valid IPv4 endpoint", () => {
  assertEquals(isValidEndpoint("1.2.3.4:31820"), true);
});

Deno.test("isValidEndpoint - valid IPv6 endpoint", () => {
  assertEquals(isValidEndpoint("[::1]:31820"), true);
});

Deno.test("isValidEndpoint - rejects invalid port", () => {
  assertEquals(isValidEndpoint("1.2.3.4:0"), false);
});

Deno.test("isValidEndpoint - rejects no port", () => {
  assertEquals(isValidEndpoint("1.2.3.4"), false);
});

Deno.test("isValidEndpoint - rejects port > 65535", () => {
  assertEquals(isValidEndpoint("1.2.3.4:99999"), false);
});
