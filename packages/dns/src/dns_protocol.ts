/**
 * DNS protocol implementation (RFC 1035)
 *
 * Handles parsing DNS queries and building DNS responses
 */

import type { DnsQuery, DnsResponse } from "./types.ts";
import { DnsQueryType, DnsResponseCode } from "./types.ts";

/**
 * Concatenate multiple Uint8Array parts into a single array
 */
function concatUint8Arrays(parts: Uint8Array[]): Uint8Array {
  const totalLength = parts.reduce((sum, p) => sum + p.length, 0);
  const result = new Uint8Array(totalLength);
  let offset = 0;
  for (const part of parts) {
    result.set(part, offset);
    offset += part.length;
  }
  return result;
}

/**
 * Parse a DNS query packet
 *
 * DNS packet format:
 * - Header (12 bytes)
 *   - Transaction ID (2 bytes)
 *   - Flags (2 bytes)
 *   - Question count (2 bytes)
 *   - Answer count (2 bytes)
 *   - Authority count (2 bytes)
 *   - Additional count (2 bytes)
 * - Question section
 *   - Domain name (variable length, label format)
 *   - Query type (2 bytes)
 *   - Query class (2 bytes)
 *
 * @param data Raw UDP packet data
 * @returns Parsed DNS query
 */
export function parseDnsQuery(data: Uint8Array): DnsQuery {
  if (data.length < 12) {
    throw new Error("DNS packet too short");
  }

  const view = new DataView(data.buffer, data.byteOffset, data.byteLength);

  // Parse header
  const transactionId = view.getUint16(0);
  const flags = view.getUint16(2);
  const questionCount = view.getUint16(4);

  // We only handle queries with exactly 1 question
  if (questionCount !== 1) {
    throw new Error(`Unsupported question count: ${questionCount}`);
  }

  // Check this is a query (QR bit = 0)
  const isQuery = (flags & 0x8000) === 0;
  if (!isQuery) {
    throw new Error("Not a DNS query");
  }

  // Parse question section starting at byte 12
  let offset = 12;
  const { domain, newOffset } = parseDomainName(data, offset);
  offset = newOffset;

  // Query type and class
  if (offset + 4 > data.length) {
    throw new Error("DNS packet truncated");
  }

  const queryType = view.getUint16(offset) as DnsQueryType;
  const queryClass = view.getUint16(offset + 2);

  return {
    transactionId,
    flags,
    domain,
    queryType,
    queryClass,
    raw: data,
  };
}

/**
 * Parse a domain name from DNS packet (label format)
 *
 * Domain names are encoded as a sequence of labels:
 * - Each label is prefixed with its length (1 byte)
 * - Labels are separated by dots when decoded
 * - Terminated by a null byte (0)
 *
 * @param data Full packet data
 * @param offset Starting offset
 * @returns Parsed domain and new offset
 */
function parseDomainName(
  data: Uint8Array,
  offset: number,
): { domain: string; newOffset: number } {
  const labels: string[] = [];
  let currentOffset = offset;

  while (currentOffset < data.length) {
    const length = data[currentOffset];

    // Check for null terminator
    if (length === 0) {
      currentOffset++;
      break;
    }

    // Check for compression pointer (starts with 11xxxxxx)
    if ((length & 0xc0) === 0xc0) {
      // Compression pointer - 2 bytes
      const pointer = ((length & 0x3f) << 8) | data[currentOffset + 1];
      const { domain: compressedDomain } = parseDomainName(data, pointer);
      labels.push(compressedDomain);
      currentOffset += 2;
      break;
    }

    // Regular label
    currentOffset++;
    if (currentOffset + length > data.length) {
      throw new Error("Domain name extends beyond packet");
    }

    const label = new TextDecoder().decode(data.subarray(currentOffset, currentOffset + length));
    labels.push(label);
    currentOffset += length;
  }

  return {
    domain: labels.join("."),
    newOffset: currentOffset,
  };
}

/**
 * Build a DNS response packet
 *
 * @param response Response data
 * @returns Raw UDP packet data
 */
export function buildDnsResponse(response: DnsResponse): Uint8Array {
  const parts: Uint8Array[] = [];

  // Header
  const header = new Uint8Array(12);
  const headerView = new DataView(header.buffer);

  // Transaction ID
  headerView.setUint16(0, response.transactionId);

  // Flags: QR=1 (response), OPCODE=0 (standard), AA=1 (authoritative),
  //        TC=0 (not truncated), RD=1 (recursion desired),
  //        RA=0 (no recursion), Z=0, RCODE from response
  const flags = 0x8400 | (response.responseCode & 0x0f); // QR=1, AA=1
  headerView.setUint16(2, flags);

  // Question count
  headerView.setUint16(4, 1);

  // Answer count
  headerView.setUint16(6, response.ips.length);

  // Authority and additional counts
  headerView.setUint16(8, 0);
  headerView.setUint16(10, 0);

  parts.push(header);

  // Question section (echo back the query)
  const questionDomain = encodeDomainName(response.domain);
  const questionMeta = new Uint8Array(4);
  const questionView = new DataView(questionMeta.buffer);
  questionView.setUint16(0, response.queryType); // Echo back the original query type
  questionView.setUint16(2, 1); // IN class

  parts.push(questionDomain);
  parts.push(questionMeta);

  // Answer section - one A record per IP
  for (const ip of response.ips) {
    // Name - use compression pointer to question domain (offset 12)
    const namePointer = new Uint8Array([0xc0, 0x0c]);
    parts.push(namePointer);

    // Type, Class, TTL, Data length
    const answerMeta = new Uint8Array(10);
    const answerView = new DataView(answerMeta.buffer);
    answerView.setUint16(0, DnsQueryType.A); // Type: A
    answerView.setUint16(2, 1); // Class: IN
    answerView.setUint32(4, response.ttl); // TTL
    answerView.setUint16(8, 4); // RDLENGTH: 4 bytes for IPv4

    parts.push(answerMeta);

    // IPv4 address
    const ipParts = ip.split(".").map((p) => parseInt(p, 10));
    const ipBytes = new Uint8Array(ipParts);
    parts.push(ipBytes);
  }

  return concatUint8Arrays(parts);
}

/**
 * Build an NXDOMAIN response (domain not found)
 *
 * @param query Original query
 * @returns Raw UDP packet data
 */
export function buildNxdomainResponse(query: DnsQuery): Uint8Array {
  return buildDnsResponse({
    transactionId: query.transactionId,
    responseCode: DnsResponseCode.NXDOMAIN,
    domain: query.domain,
    ips: [],
    ttl: 60,
    queryType: query.queryType,
  });
}

/**
 * Build a SERVFAIL response (server failure)
 *
 * @param query Original query
 * @returns Raw UDP packet data
 */
export function buildServfailResponse(query: DnsQuery): Uint8Array {
  return buildDnsResponse({
    transactionId: query.transactionId,
    responseCode: DnsResponseCode.SERVFAIL,
    domain: query.domain,
    ips: [],
    ttl: 0,
    queryType: query.queryType,
  });
}

/**
 * Encode a domain name to DNS label format
 *
 * @param domain Domain name (e.g., "example.jiji")
 * @returns Encoded bytes
 */
export function encodeDomainName(domain: string): Uint8Array {
  const labels = domain.split(".");
  const parts: Uint8Array[] = [];

  for (const label of labels) {
    if (label.length > 63) {
      throw new Error(`Label too long: ${label}`);
    }
    const labelBytes = new TextEncoder().encode(label);
    const labelLength = new Uint8Array([labelBytes.length]);
    parts.push(labelLength);
    parts.push(labelBytes);
  }

  // Null terminator
  parts.push(new Uint8Array([0]));

  return concatUint8Arrays(parts);
}

/**
 * Check if a domain matches the service domain suffix
 *
 * @param domain Domain from query
 * @param serviceDomain Service domain suffix (e.g., "jiji")
 * @returns True if domain ends with .serviceDomain
 */
export function isServiceDomain(domain: string, serviceDomain: string): boolean {
  const suffix = `.${serviceDomain}`;
  return domain.toLowerCase().endsWith(suffix) ||
    domain.toLowerCase() === serviceDomain;
}

/**
 * Extract the hostname part from a service domain query
 *
 * @param domain Full domain (e.g., "casa-api.jiji")
 * @param serviceDomain Service domain suffix (e.g., "jiji")
 * @returns Hostname (e.g., "casa-api")
 */
export function extractHostname(domain: string, serviceDomain: string): string {
  const suffix = `.${serviceDomain}`;
  if (domain.toLowerCase().endsWith(suffix)) {
    return domain.slice(0, -suffix.length).toLowerCase();
  }
  return domain.toLowerCase();
}
