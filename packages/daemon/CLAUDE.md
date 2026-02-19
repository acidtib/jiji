# Daemon Package

Network reconciliation daemon (formerly jiji-control-loop). Runs continuously on
each server maintaining mesh health and cluster state.

```bash
deno task check          # fmt + lint + test
deno task dev            # Run with watch mode (requires JIJI_SERVER_ID env)
deno task build          # Compile to build/jiji-daemon
```

Requires `JIJI_SERVER_ID` env var to run. See `src/types.ts` `parseConfig()` for
all env vars (all prefixed with `JIJI_`).

## Main Loop (src/main.ts)

Every 30s iteration:

1. Heartbeat update
2. WireGuard peer reconciliation
3. Peer health monitoring
4. Container health sync
5. Garbage collection (every 10 iterations)
6. Public IP discovery (every 20 iterations)
7. Corrosion health check (every 20 iterations)
8. Split-brain detection (every 20 iterations)

## Key Files

- `src/peer_reconciler.ts` - Add/remove WireGuard peers from Corrosion state
- `src/peer_monitor.ts` - Check handshake times, rotate endpoints
- `src/container_health.ts` - TCP health checks, update Corrosion
- `src/corrosion_client.ts` - HTTP client for Corrosion API
- `src/corrosion_cli.ts` - CLI wrapper for `corrosion query`
