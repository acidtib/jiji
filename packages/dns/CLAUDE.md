# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this
repository.

## Commands

```bash
deno task check          # fmt + lint + test
deno task dev            # Run with watch mode (requires JIJI_LISTEN_ADDR)
deno task run            # Run without watch mode
deno task build          # Compile to build/jiji-dns
deno test --allow-net --allow-read --allow-env tests/dns_cache_test.ts  # Single test file
```

## Environment Variables

| Variable                  | Required | Default                 | Description                                                         |
| ------------------------- | -------- | ----------------------- | ------------------------------------------------------------------- |
| `JIJI_LISTEN_ADDR`        | Yes      | —                       | Comma-separated `host:port` (e.g., `10.210.1.1:53,10.210.128.1:53`) |
| `JIJI_CORROSION_API`      | No       | `http://127.0.0.1:9220` | Corrosion HTTP API address                                          |
| `JIJI_SERVICE_DOMAIN`     | No       | `jiji`                  | Domain suffix for service queries                                   |
| `JIJI_DNS_TTL`            | No       | `60`                    | TTL for DNS responses (seconds)                                     |
| `JIJI_RECONNECT_INTERVAL` | No       | `5000`                  | Base reconnect delay (ms, with exponential backoff)                 |

## Architecture

```
Corrosion DB ─HTTP Stream─> CorrosionSubscriber ─> DnsCache <── DnsServer <── UDP Queries
                           (NDJSON events)        (in-memory)   (port 53)
```

- `src/corrosion_subscriber.ts` — Streams NDJSON from Corrosion `/v1/subscriptions`. Message types:
  `columns`, `row` (initial sync), `change` (insert/update/delete), `eoq` (sync complete).
  Auto-reconnects with exponential backoff + jitter (max 60s).
- `src/dns_cache.ts` — Dual-indexed cache (`byHostname` + `byContainerId`). Only healthy containers
  returned. Newest-container-wins per service/server combination (by `startedAt`).
- `src/dns_server.ts` — UDP DNS server (RFC 1035). Routes `*.{serviceDomain}` to cache, forwards
  everything else to system resolvers from `/etc/resolv.conf` (skips localhost and own IPs). Only A
  records supported; AAAA queries return empty NOERROR.
- `src/dns_protocol.ts` — DNS wire format: packet parsing, domain name encoding (label format with
  compression pointer support), response building.
- `src/types.ts` — All type definitions and `parseConfig()`.

## DNS Resolution

- `{project}-{service}.{domain}` → all healthy container IPs (e.g., `casa-api.jiji`)
- `{project}-{service}-{instanceId}.{domain}` → specific instance (e.g., `casa-api-primary.jiji`)

Hostnames are case-insensitive. Cache generates both primary and instance-specific hostnames per
record.

## Gotchas

- `unstable: ["net"]` is required for `Deno.listenDatagram()` but lives in **root** `deno.json`, not
  here (Deno workspace rule)
- Custom `fmt` config (lineWidth: 100) differs from CLI package defaults
- The Corrosion subscription SQL joins `containers` with `services` table to get `project` — both
  tables must exist in Corrosion
- Forwarded DNS queries use ephemeral UDP sockets with 5s timeout per resolver
