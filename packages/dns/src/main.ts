/**
 * jiji-dns - DNS server for Jiji service discovery
 *
 * Uses Corrosion subscriptions for real-time container updates
 * and serves DNS queries for the .jiji domain.
 */

import { parseConfig } from "./types.ts";
import { DnsCache } from "./dns_cache.ts";
import { DnsServer } from "./dns_server.ts";
import { CorrosionSubscriber } from "./corrosion_subscriber.ts";

// ASCII art banner
const BANNER = `
     _ _ _ _         _
    (_|_|_|_)   __  | |_ __  ___
    | | | | |  / _\` | | '_ \\/ __|
    | | | | | | (_| | | | | \\__ \\
   _/ |_|_|_|  \\__,_|_|_| |_|___/
  |__/
`;

async function main(): Promise<void> {
  console.log(BANNER);

  // Parse configuration
  let config;
  try {
    config = parseConfig();
  } catch (error) {
    console.error("Configuration error:", error);
    Deno.exit(1);
  }

  console.log(`Configuration:`);
  console.log(`  Listen addresses: ${config.listenAddrs.join(", ")}`);
  console.log(`  Service domain: ${config.serviceDomain}`);
  console.log(`  Corrosion API:  ${config.corrosionApi}`);
  console.log(`  TTL:            ${config.ttl}s`);
  console.log("");

  // Initialize components
  const cache = new DnsCache();
  const server = new DnsServer(config, cache);

  let isReady = false;

  // Create subscriber with event handlers
  const subscriber = new CorrosionSubscriber(
    config.corrosionApi,
    {
      onUpsert: (record) => {
        cache.set(record);
        if (isReady) {
          console.log(
            `[UPSERT] ${record.project}-${record.service} -> ${record.ip} (container: ${
              record.containerId.slice(0, 12)
            })`,
          );
        }
      },
      onDelete: (containerId) => {
        const record = cache.getByContainerId(containerId);
        if (record) {
          console.log(
            `[DELETE] ${record.project}-${record.service} (container: ${containerId.slice(0, 12)})`,
          );
        }
        cache.remove(containerId);
      },
      onReady: () => {
        isReady = true;
        const stats = cache.getStats();
        console.log(
          `[READY] Initial sync complete: ${stats.totalRecords} records, ${stats.hostnames} hostnames`,
        );
      },
      onError: (error) => {
        console.error(`[ERROR] Corrosion subscription error: ${error.message}`);
      },
      onReconnect: (attempt) => {
        console.log(`[RECONNECT] Attempting reconnection #${attempt}...`);
      },
    },
    config.reconnectInterval,
  );

  // Handle shutdown signals
  const shutdown = () => {
    console.log("\nShutting down...");
    subscriber.stop();
    server.stop();
    Deno.exit(0);
  };

  // Register signal handlers
  Deno.addSignalListener("SIGINT", shutdown);
  Deno.addSignalListener("SIGTERM", shutdown);

  // Start components
  console.log("Starting Corrosion subscriber...");
  subscriber.start().catch((error) => {
    console.error("Subscriber error:", error);
  });

  console.log("Starting DNS server...");
  try {
    await server.start();
  } catch (error) {
    console.error("DNS server error:", error);
    subscriber.stop();
    Deno.exit(1);
  }
}

// Run
main().catch((error) => {
  console.error("Fatal error:", error);
  Deno.exit(1);
});
