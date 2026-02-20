/**
 * Type definitions for jiji-dns
 */

/**
 * DNS record stored in the cache
 */
export interface DnsRecord {
  /** Container ID from Corrosion */
  containerId: string;
  /** Service name (e.g., "api", "web") */
  service: string;
  /** Project name (e.g., "casa", "myapp") */
  project: string;
  /** Server ID where container runs */
  serverId: string;
  /** Container IP address */
  ip: string;
  /** Whether container is healthy */
  healthy: boolean;
  /** Container start timestamp */
  startedAt: number;
  /** Optional instance identifier for multi-server deployments */
  instanceId?: string;
}

/**
 * DNS query types (RFC 1035)
 */
export enum DnsQueryType {
  A = 1, // IPv4 address
  AAAA = 28, // IPv6 address
  CNAME = 5, // Canonical name
  MX = 15, // Mail exchange
  TXT = 16, // Text record
  NS = 2, // Name server
  SOA = 6, // Start of authority
  PTR = 12, // Pointer record
}

/**
 * DNS response codes (RFC 1035)
 */
export enum DnsResponseCode {
  NOERROR = 0, // No error
  FORMERR = 1, // Format error
  SERVFAIL = 2, // Server failure
  NXDOMAIN = 3, // Name does not exist
  NOTIMP = 4, // Not implemented
  REFUSED = 5, // Query refused
}

/**
 * Parsed DNS query
 */
export interface DnsQuery {
  /** Transaction ID for matching response */
  transactionId: number;
  /** Query flags */
  flags: number;
  /** Domain name being queried */
  domain: string;
  /** Query type (A, AAAA, etc.) */
  queryType: DnsQueryType;
  /** Query class (usually IN = 1) */
  queryClass: number;
  /** Raw query data for forwarding */
  raw: Uint8Array;
}

/**
 * DNS response to send
 */
export interface DnsResponse {
  /** Transaction ID matching the query */
  transactionId: number;
  /** Response code */
  responseCode: DnsResponseCode;
  /** Domain name */
  domain: string;
  /** IP addresses (for A/AAAA records) */
  ips: string[];
  /** Time to live in seconds */
  ttl: number;
  /** Query type to echo back (must match the query) */
  queryType: DnsQueryType;
}

/**
 * Corrosion subscription message types
 */

/** Initial columns message */
export interface CorrosionColumnsMessage {
  columns: string[];
}

/** Single row message with row index and values */
export interface CorrosionRowMessage {
  row: [number, (string | number | null)[]];
}

/**
 * Corrosion change event format
 *
 * Corrosion sends change events as arrays:
 * - Insert: ["insert", rowIndex, [values...], changeId]
 * - Update: ["update", rowIndex, [values...], changeId]
 * - Delete: ["delete", rowIndex, [primaryKeys...], changeId]
 */
export type CorrosionChangeArray = [
  "insert" | "update" | "delete",
  number,
  (string | number | null)[],
  number,
];

/** Change message wrapper */
export interface CorrosionChangeMessage {
  change: CorrosionChangeArray;
}

/** End of query marker */
export interface CorrosionEoqMessage {
  eoq: {
    time: number;
    change_id: number;
  };
}

/** Union type for all Corrosion subscription messages */
export type CorrosionMessage =
  | CorrosionColumnsMessage
  | CorrosionRowMessage
  | CorrosionChangeMessage
  | CorrosionEoqMessage;

/**
 * Event types emitted by CorrosionSubscriber
 */
export interface SubscriberEvents {
  /** Called when a container is added or updated */
  onUpsert: (record: DnsRecord) => void;
  /** Called when a container is removed */
  onDelete: (containerId: string) => void;
  /** Called when initial sync is complete */
  onReady: () => void;
  /** Called on connection error */
  onError: (error: Error) => void;
  /** Called when reconnecting */
  onReconnect: (attempt: number) => void;
}

/**
 * Configuration for the DNS server
 */
export interface DnsServerConfig {
  /** Addresses to listen on (e.g., ["10.210.1.1:53", "10.210.128.1:53"]) */
  listenAddrs: string[];
  /** Service domain suffix (e.g., "jiji") */
  serviceDomain: string;
  /** Corrosion API address (e.g., "http://127.0.0.1:31220") */
  corrosionApi: string;
  /** TTL for DNS responses in seconds (default: 60) */
  ttl?: number;
  /** Reconnect interval in milliseconds (default: 5000) */
  reconnectInterval?: number;
  /** Maximum reconnect attempts before giving up (default: unlimited) */
  maxReconnectAttempts?: number;
}

/**
 * Parse configuration from environment variables
 */
export function parseConfig(): DnsServerConfig {
  const listenAddrEnv = Deno.env.get("JIJI_LISTEN_ADDR");
  const serviceDomain = Deno.env.get("JIJI_SERVICE_DOMAIN") || "jiji";
  const corrosionApi = Deno.env.get("JIJI_CORROSION_API") || "http://127.0.0.1:31220";
  const ttl = parseInt(Deno.env.get("JIJI_DNS_TTL") || "60", 10);
  const reconnectInterval = parseInt(Deno.env.get("JIJI_RECONNECT_INTERVAL") || "5000", 10);

  if (!listenAddrEnv) {
    throw new Error("JIJI_LISTEN_ADDR environment variable is required");
  }

  // Support comma-separated addresses (e.g., "10.210.0.1:53,10.210.128.1:53")
  const listenAddrs = listenAddrEnv.split(",").map((addr) => addr.trim());

  return {
    listenAddrs,
    serviceDomain,
    corrosionApi,
    ttl,
    reconnectInterval,
  };
}
