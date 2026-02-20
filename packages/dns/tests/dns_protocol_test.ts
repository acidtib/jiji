/**
 * Tests for DNS protocol parsing and building
 */

import { assertEquals, assertThrows } from "@std/assert";
import {
  buildDnsResponse,
  encodeDomainName,
  extractHostname,
  isServiceDomain,
  parseDnsQuery,
} from "../src/dns_protocol.ts";
import { DnsQueryType, DnsResponseCode } from "../src/types.ts";

Deno.test("encodeDomainName - simple domain", () => {
  const encoded = encodeDomainName("example.com");
  // Expected: [7]example[3]com[0]
  assertEquals(encoded[0], 7); // "example" length
  assertEquals(new TextDecoder().decode(encoded.subarray(1, 8)), "example");
  assertEquals(encoded[8], 3); // "com" length
  assertEquals(new TextDecoder().decode(encoded.subarray(9, 12)), "com");
  assertEquals(encoded[12], 0); // null terminator
});

Deno.test("encodeDomainName - service domain", () => {
  const encoded = encodeDomainName("casa-api.jiji");
  // Expected: [8]casa-api[4]jiji[0]
  assertEquals(encoded[0], 8);
  assertEquals(new TextDecoder().decode(encoded.subarray(1, 9)), "casa-api");
  assertEquals(encoded[9], 4);
  assertEquals(new TextDecoder().decode(encoded.subarray(10, 14)), "jiji");
  assertEquals(encoded[14], 0);
});

Deno.test("isServiceDomain - matches .jiji suffix", () => {
  assertEquals(isServiceDomain("casa-api.jiji", "jiji"), true);
  assertEquals(isServiceDomain("myapp-web.jiji", "jiji"), true);
  assertEquals(isServiceDomain("CASA-API.JIJI", "jiji"), true);
});

Deno.test("isServiceDomain - does not match other domains", () => {
  assertEquals(isServiceDomain("google.com", "jiji"), false);
  assertEquals(isServiceDomain("example.jijii", "jiji"), false);
  assertEquals(isServiceDomain("casa-api.local", "jiji"), false);
});

Deno.test("extractHostname - extracts hostname from service domain", () => {
  assertEquals(extractHostname("casa-api.jiji", "jiji"), "casa-api");
  assertEquals(extractHostname("myapp-web.jiji", "jiji"), "myapp-web");
  assertEquals(extractHostname("CASA-API.JIJI", "jiji"), "casa-api");
});

Deno.test("parseDnsQuery - valid A record query", () => {
  // Build a test DNS query packet for casa-api.jiji
  const domain = "casa-api.jiji";
  const packet = buildTestQuery(domain, DnsQueryType.A);

  const query = parseDnsQuery(packet);

  assertEquals(query.transactionId, 0x1234);
  assertEquals(query.domain, domain);
  assertEquals(query.queryType, DnsQueryType.A);
  assertEquals(query.queryClass, 1);
});

Deno.test("parseDnsQuery - rejects non-query packets", () => {
  // Build a response packet (QR bit = 1)
  const packet = buildTestQuery("test.jiji", DnsQueryType.A);
  // Set QR bit to 1 (response)
  packet[2] = 0x80;

  assertThrows(() => parseDnsQuery(packet), Error, "Not a DNS query");
});

Deno.test("parseDnsQuery - rejects too short packet", () => {
  const shortPacket = new Uint8Array(10);
  assertThrows(() => parseDnsQuery(shortPacket), Error, "DNS packet too short");
});

Deno.test("buildDnsResponse - single IP response", () => {
  const response = buildDnsResponse({
    transactionId: 0x5678,
    responseCode: DnsResponseCode.NOERROR,
    domain: "casa-api.jiji",
    ips: ["10.210.1.5"],
    ttl: 60,
    queryType: DnsQueryType.A,
  });

  // Verify it's a valid DNS packet
  assertEquals(response.length > 12, true);

  // Check transaction ID
  assertEquals((response[0] << 8) | response[1], 0x5678);

  // Check flags (should be response with authoritative answer)
  const flags = (response[2] << 8) | response[3];
  assertEquals((flags & 0x8000) !== 0, true); // QR bit set
  assertEquals((flags & 0x0400) !== 0, true); // AA bit set

  // Check answer count
  const answerCount = (response[6] << 8) | response[7];
  assertEquals(answerCount, 1);
});

Deno.test("buildDnsResponse - multiple IPs", () => {
  const response = buildDnsResponse({
    transactionId: 0x9abc,
    responseCode: DnsResponseCode.NOERROR,
    domain: "casa-api.jiji",
    ips: ["10.210.1.5", "10.210.2.3"],
    ttl: 60,
    queryType: DnsQueryType.A,
  });

  // Check answer count
  const answerCount = (response[6] << 8) | response[7];
  assertEquals(answerCount, 2);
});

Deno.test("buildDnsResponse - NXDOMAIN", () => {
  const response = buildDnsResponse({
    transactionId: 0xdef0,
    responseCode: DnsResponseCode.NXDOMAIN,
    domain: "unknown.jiji",
    ips: [],
    ttl: 60,
    queryType: DnsQueryType.A,
  });

  // Check response code
  const rcode = response[3] & 0x0f;
  assertEquals(rcode, DnsResponseCode.NXDOMAIN);

  // Check answer count is 0
  const answerCount = (response[6] << 8) | response[7];
  assertEquals(answerCount, 0);
});

Deno.test("buildDnsResponse - AAAA query echoes correct query type", () => {
  const response = buildDnsResponse({
    transactionId: 0x1111,
    responseCode: DnsResponseCode.NOERROR,
    domain: "casa-api.jiji",
    ips: [], // No IPv6 addresses
    ttl: 60,
    queryType: DnsQueryType.AAAA,
  });

  // Verify it's a valid DNS packet
  assertEquals(response.length > 12, true);

  // Check transaction ID
  assertEquals((response[0] << 8) | response[1], 0x1111);

  // Check answer count is 0 (no AAAA records)
  const answerCount = (response[6] << 8) | response[7];
  assertEquals(answerCount, 0);

  // Find the question section (after 12-byte header)
  // Skip past domain name to find query type
  let offset = 12;
  while (offset < response.length && response[offset] !== 0) {
    offset += response[offset] + 1;
  }
  offset++; // Skip null terminator

  // Query type should be AAAA (28), not A (1)
  const queryType = (response[offset] << 8) | response[offset + 1];
  assertEquals(queryType, DnsQueryType.AAAA, "Query type in response should match AAAA");
});

// --- Compression pointer security tests ---

Deno.test("parseDnsQuery - self-referencing compression pointer is rejected", () => {
  // Build a packet where the question domain has a compression pointer to itself.
  // Header (12 bytes) + domain starting at offset 12:
  //   byte 12-13: compression pointer 0xC0 0x0C → points back to offset 12
  //   byte 14-15: query type A (0x0001)
  //   byte 16-17: query class IN (0x0001)
  const packet = new Uint8Array(18);
  const view = new DataView(packet.buffer);
  view.setUint16(0, 0x1234); // txn ID
  view.setUint16(2, 0x0100); // standard query
  view.setUint16(4, 1); // 1 question
  // Self-referencing pointer at offset 12 → offset 12
  packet[12] = 0xc0;
  packet[13] = 0x0c;
  view.setUint16(14, DnsQueryType.A);
  view.setUint16(16, 1);

  assertThrows(() => parseDnsQuery(packet), Error, "loop");
});

Deno.test("parseDnsQuery - forward pointer loop is rejected", () => {
  // Two compression pointers that point to each other:
  //   offset 12-13: pointer → offset 14
  //   offset 14-15: pointer → offset 12
  //   offset 16-17: query type, offset 18-19: query class
  const packet = new Uint8Array(20);
  const view = new DataView(packet.buffer);
  view.setUint16(0, 0xabcd);
  view.setUint16(2, 0x0100);
  view.setUint16(4, 1);
  // Pointer at 12 → 14
  packet[12] = 0xc0;
  packet[13] = 0x0e; // 14
  // Pointer at 14 → 12
  packet[14] = 0xc0;
  packet[15] = 0x0c; // 12
  view.setUint16(16, DnsQueryType.A);
  view.setUint16(18, 1);

  assertThrows(() => parseDnsQuery(packet), Error, "loop");
});

Deno.test("parseDnsQuery - valid compression pointer still works", () => {
  // Packet layout:
  //   0-11:  Header (12 bytes)
  //   12-15: label "api" = [3, 'a','p','i']
  //   16-17: compression pointer 0xC0 0x16 → offset 22
  //   18-19: query type A (newOffset after pointer = 18)
  //   20-21: query class IN
  //   22-27: target label [4]jiji[0] (pointed to by the compression pointer)
  // Parsed domain = "api.jiji"
  const packet = new Uint8Array(28);
  const view = new DataView(packet.buffer);
  view.setUint16(0, 0x5555);
  view.setUint16(2, 0x0100);
  view.setUint16(4, 1);

  // offset 12: label "api"
  packet[12] = 3;
  packet[13] = 0x61; // 'a'
  packet[14] = 0x70; // 'p'
  packet[15] = 0x69; // 'i'
  // offset 16: compression pointer to offset 22
  packet[16] = 0xc0;
  packet[17] = 22;
  // offset 18: query type A + class IN (parseDomainName returns newOffset=18)
  view.setUint16(18, DnsQueryType.A);
  view.setUint16(20, 1);
  // offset 22: target label "jiji" + null terminator
  packet[22] = 4;
  packet[23] = 0x6a; // 'j'
  packet[24] = 0x69; // 'i'
  packet[25] = 0x6a; // 'j'
  packet[26] = 0x69; // 'i'
  packet[27] = 0; // null terminator

  const query = parseDnsQuery(packet);
  assertEquals(query.domain, "api.jiji");
  assertEquals(query.transactionId, 0x5555);
  assertEquals(query.queryType, DnsQueryType.A);
});

Deno.test("parseDnsQuery - compression pointer OOB second byte is rejected", () => {
  // Packet just long enough for header + one compression pointer byte, missing second byte
  const packet = new Uint8Array(13);
  const view = new DataView(packet.buffer);
  view.setUint16(0, 0x1234);
  view.setUint16(2, 0x0100);
  view.setUint16(4, 1);
  // Compression pointer at offset 12, but packet is only 13 bytes — second byte missing
  packet[12] = 0xc0;

  assertThrows(() => parseDnsQuery(packet), Error, "truncated");
});

Deno.test("buildDnsResponse - rejects invalid IPv4 address", () => {
  assertThrows(
    () =>
      buildDnsResponse({
        transactionId: 0x1234,
        responseCode: DnsResponseCode.NOERROR,
        domain: "test.jiji",
        ips: ["not.an.ip.addr"],
        ttl: 60,
        queryType: DnsQueryType.A,
      }),
    Error,
    "Invalid IPv4",
  );
});

/**
 * Helper to build a test DNS query packet
 */
function buildTestQuery(domain: string, queryType: DnsQueryType): Uint8Array {
  const parts: Uint8Array[] = [];

  // Header (12 bytes)
  const header = new Uint8Array(12);
  const headerView = new DataView(header.buffer);
  headerView.setUint16(0, 0x1234); // Transaction ID
  headerView.setUint16(2, 0x0100); // Standard query
  headerView.setUint16(4, 1); // 1 question
  headerView.setUint16(6, 0); // 0 answers
  headerView.setUint16(8, 0); // 0 authority
  headerView.setUint16(10, 0); // 0 additional
  parts.push(header);

  // Question section - domain name
  const domainEncoded = encodeDomainName(domain);
  parts.push(domainEncoded);

  // Question section - type and class
  const questionMeta = new Uint8Array(4);
  const questionView = new DataView(questionMeta.buffer);
  questionView.setUint16(0, queryType);
  questionView.setUint16(2, 1); // IN class
  parts.push(questionMeta);

  // Concatenate
  const totalLength = parts.reduce((sum, p) => sum + p.length, 0);
  const result = new Uint8Array(totalLength);
  let offset = 0;
  for (const part of parts) {
    result.set(part, offset);
    offset += part.length;
  }

  return result;
}
