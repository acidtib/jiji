/**
 * UDP DNS server for service discovery
 *
 * Handles DNS queries:
 * - *.{serviceDomain} queries → lookup in cache
 * - Other queries → forward to system resolver
 */

import type { DnsServerConfig } from "./types.ts";
import { DnsQueryType } from "./types.ts";
import { DnsCache } from "./dns_cache.ts";
import {
  buildDnsResponse,
  buildServfailResponse,
  extractHostname,
  isServiceDomain,
  parseDnsQuery,
} from "./dns_protocol.ts";
import { DnsResponseCode } from "./types.ts";

/**
 * DNS server that handles queries and routes them to cache or upstream
 */
export class DnsServer {
  private config: DnsServerConfig;
  private cache: DnsCache;
  private listeners: Deno.DatagramConn[] = [];
  private isRunning = false;
  private systemResolvers: string[] = [];

  constructor(config: DnsServerConfig, cache: DnsCache) {
    this.config = config;
    this.cache = cache;
  }

  /**
   * Start the DNS server
   */
  async start(): Promise<void> {
    // Load system resolvers
    this.systemResolvers = await this.loadSystemResolvers();
    console.log(`System resolvers: ${this.systemResolvers.join(", ")}`);

    // Bind to all listen addresses
    for (const listenAddr of this.config.listenAddrs) {
      const [host, portStr] = listenAddr.split(":");
      const port = parseInt(portStr, 10);

      const listener = Deno.listenDatagram({
        port,
        hostname: host,
        transport: "udp",
      });
      this.listeners.push(listener);
      console.log(`DNS server listening on ${listenAddr}`);
    }

    this.isRunning = true;

    // Handle incoming queries on all listeners
    await this.handleQueries();
  }

  /**
   * Stop the DNS server
   */
  stop(): void {
    this.isRunning = false;
    for (const listener of this.listeners) {
      listener.close();
    }
    this.listeners = [];
  }

  /**
   * Main query handling loop for all listeners
   */
  private async handleQueries(): Promise<void> {
    if (this.listeners.length === 0) {
      return;
    }

    // Create a handler for each listener
    const handlers = this.listeners.map((listener) => this.handleListenerQueries(listener));

    // Wait for all handlers (they run until stopped)
    await Promise.all(handlers);
  }

  /**
   * Handle queries for a single listener
   */
  private async handleListenerQueries(listener: Deno.DatagramConn): Promise<void> {
    while (this.isRunning) {
      try {
        const [data, remoteAddr] = await listener.receive();
        // Handle query in background
        this.handleQuery(listener, data, remoteAddr).catch((error) => {
          console.error("Error handling query:", error);
        });
      } catch (error) {
        if (this.isRunning) {
          console.error("Error receiving packet:", error);
        }
      }
    }
  }

  /**
   * Handle a single DNS query
   */
  private async handleQuery(
    listener: Deno.DatagramConn,
    data: Uint8Array,
    remoteAddr: Deno.Addr,
  ): Promise<void> {
    let response: Uint8Array;

    try {
      const query = parseDnsQuery(data);

      // Check if this is a query for our service domain
      if (isServiceDomain(query.domain, this.config.serviceDomain)) {
        response = this.handleServiceQuery(query.domain, query.transactionId, query.queryType);
      } else {
        // Forward to upstream resolver
        response = await this.forwardQuery(data);
      }
    } catch (error) {
      console.error("Error processing query:", error);
      // Try to send SERVFAIL response
      try {
        const query = parseDnsQuery(data);
        response = buildServfailResponse(query);
      } catch {
        // Can't even parse the query, nothing to send
        return;
      }
    }

    // Send response
    try {
      await listener.send(response, remoteAddr);
    } catch (error) {
      console.error("Error sending response:", error);
    }
  }

  /**
   * Handle a query for our service domain
   */
  private handleServiceQuery(
    domain: string,
    transactionId: number,
    queryType: DnsQueryType,
  ): Uint8Array {
    const ttl = this.config.ttl ?? 60;

    // We only handle A record queries - return empty response for other types (AAAA, etc.)
    if (queryType !== DnsQueryType.A) {
      return buildDnsResponse({
        transactionId,
        responseCode: DnsResponseCode.NOERROR,
        domain,
        ips: [],
        ttl,
        queryType, // Echo back the original query type
      });
    }

    // Extract hostname and lookup in cache
    const hostname = extractHostname(domain, this.config.serviceDomain);
    const ips = this.cache.get(hostname);

    // Return NXDOMAIN if no records found, otherwise return all IPs
    const responseCode = ips.length === 0 ? DnsResponseCode.NXDOMAIN : DnsResponseCode.NOERROR;

    return buildDnsResponse({
      transactionId,
      responseCode,
      domain,
      ips,
      ttl,
      queryType: DnsQueryType.A,
    });
  }

  /**
   * Forward a query to upstream DNS resolver
   */
  private async forwardQuery(queryData: Uint8Array): Promise<Uint8Array> {
    if (this.systemResolvers.length === 0) {
      throw new Error("No system resolvers available");
    }

    // Try each resolver in order
    for (const resolver of this.systemResolvers) {
      try {
        return await this.queryResolver(queryData, resolver);
      } catch (error) {
        console.warn(`Resolver ${resolver} failed:`, error);
        continue;
      }
    }

    throw new Error("All resolvers failed");
  }

  /**
   * Send query to a single resolver and wait for response
   */
  private async queryResolver(queryData: Uint8Array, resolver: string): Promise<Uint8Array> {
    const conn = Deno.listenDatagram({
      port: 0, // Ephemeral port
      hostname: "0.0.0.0",
      transport: "udp",
    });

    try {
      // Send query
      const resolverAddr: Deno.NetAddr = {
        transport: "udp",
        hostname: resolver,
        port: 53,
      };
      await conn.send(queryData, resolverAddr);

      // Wait for response with timeout
      const responsePromise = conn.receive();
      const timeoutPromise = new Promise<never>((_, reject) => {
        setTimeout(() => reject(new Error("Resolver timeout")), 5000);
      });

      const [response] = await Promise.race([responsePromise, timeoutPromise]);
      return response;
    } finally {
      conn.close();
    }
  }

  /**
   * Load system DNS resolvers from /etc/resolv.conf
   */
  private async loadSystemResolvers(): Promise<string[]> {
    const fallbackResolvers = ["8.8.8.8", "1.1.1.1"];

    try {
      const content = await Deno.readTextFile("/etc/resolv.conf");

      // Get all our listen IPs for filtering
      const ourIps = new Set(this.config.listenAddrs.map((addr) => addr.split(":")[0]));
      const skipIps = new Set(["127.0.0.1", "::1", ...ourIps]);

      const resolvers = content
        .split("\n")
        .filter((line) => line.trim().startsWith("nameserver"))
        .map((line) => line.trim().split(/\s+/)[1])
        .filter((ip): ip is string => ip !== undefined && !skipIps.has(ip));

      return resolvers.length > 0 ? resolvers : fallbackResolvers;
    } catch {
      return fallbackResolvers;
    }
  }

  /**
   * Get cache statistics
   */
  getStats(): { cacheStats: ReturnType<DnsCache["getStats"]>; resolvers: string[] } {
    return {
      cacheStats: this.cache.getStats(),
      resolvers: this.systemResolvers,
    };
  }
}
