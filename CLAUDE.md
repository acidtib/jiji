# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with
code in this repository.

## Monorepo Structure

This is a Deno workspace monorepo with three packages:

```
packages/
├── cli/       # @jiji/cli — Main CLI tool (Cliffy, SSH, deployment)
├── dns/       # @jiji/dns — DNS server for service discovery
└── daemon/    # @jiji/daemon — Network reconciliation daemon
```

All packages share a unified version number managed by `./bin/version`.

## Workspace Conventions

- `nodeModulesDir` and `unstable` must be in root `deno.json` (Deno workspace
  requirement) — member configs cannot override these
- `@std/assert` is declared in root `deno.json` and inherited by all members —
  don't re-declare it in package configs
- `allowScripts` for npm packages with native builds lives in root config
- Tests run from the package directory (`cd packages/cli && deno task check`),
  so relative paths in tests resolve from there

## Quick Start Commands

```bash
# Install dependencies (run from repo root)
deno install --allow-scripts=npm:cpu-features,npm:ssh2

# Run checks for a specific package
cd packages/cli && deno task check
cd packages/dns && deno task check
cd packages/daemon && deno task check

# Run a single test file
deno test --allow-all packages/cli/tests/deploy_plan_test.ts

# Build binaries
cd packages/cli && deno task build       # → build/jiji
cd packages/dns && deno task build       # → build/jiji-dns
cd packages/daemon && deno task build    # → build/jiji-daemon

# CLI-specific
cd packages/cli && deno task run         # Run CLI directly
cd packages/cli && deno task install     # Build and install to /usr/local/bin/jiji

# Version management (run from repo root)
./bin/version              # Show current version
./bin/version --bump       # Auto-increment patch version
./bin/version --bump 1.0.0 # Set specific version (updates all 3 packages)
```

## High-Level Architecture

### Command Flow (CLI)

The CLI uses Cliffy for command routing. Each command follows this pattern:

```
Command → setupCommandContext() → Load Config → Filter Hosts/Services → SSH Setup → Execute → Cleanup
```

Commands are defined in `packages/cli/src/main.ts` and handlers live in
`packages/cli/src/commands/`. The `setupCommandContext()` helper in
`packages/cli/src/utils/command_helpers.ts` handles common setup.

### Configuration System

Configuration classes in `packages/cli/src/lib/configuration/` use
**lazy-loaded, cached properties**. Validation happens at property access time,
not construction.

All configuration classes extend `BaseConfiguration`. The main classes are:

- `Configuration` - Main entry point, accesses other configs via getters
- `BuilderConfiguration` - Build settings, accesses `RegistryConfiguration`
- `RegistryConfiguration` - Registry authentication and server settings
- `SSHConfiguration` - SSH connection settings
- `NetworkConfiguration` - WireGuard mesh network settings
- `ServersConfiguration` - Server definitions
- `ServiceConfiguration` - Per-service config, accesses `ProxyConfiguration`
- `ProxyConfiguration` - kamal-proxy routing and health checks
- `EnvironmentConfiguration` - Environment variables (shared and per-service)

Config loader searches upward from cwd for `.jiji/deploy.yml` or
`jiji.{environment}.yml`.

### Zero-Downtime Deployment Strategy

Critical flow in `ContainerDeploymentService`:

1. Rename old container (`web` → `web_old_{timestamp}`) - keeps it running
2. Deploy new container with original name
3. Wait for health checks (proxy endpoint or container readiness)
4. Update proxy routing to new container
5. Stop and remove old container
6. Cleanup old images (keep N versions based on `retain` setting)

If health checks fail, old container continues serving traffic.

### Distributed Networking Stack

Three components work together in `packages/cli/src/lib/network/`:

**WireGuard** (`wireguard.ts`): Encrypted mesh VPN

- Per-server /24 subnet from cluster CIDR (e.g., 10.210.0.0/16)
- IPv6 CRDT identifiers for unique peer identification

**Corrosion** (`corrosion.ts`): Distributed CRDT-based SQLite

- No central coordinator - eventual consistency
- Tables: `servers`, `containers`, `services`, `cluster_metadata`
- Gossip on port 31280, API on port 31220

**jiji-dns** (`dns.ts`): DNS-based service discovery

- Resolves `<project>-<service>.jiji` to container IPs
- Queries Corrosion for healthy containers
- Daemon-level DNS configuration (all containers auto-configured)

### SSH Connection Management

`SSHConnectionPool` in `packages/cli/src/utils/ssh_pool.ts` uses a semaphore
for concurrency control (default: 30 concurrent connections).

**Patterns:**

- `executeConcurrent()` - Acquire permit before SSH operation, prevents server
  overload
- `executeBestEffort()` - For cleanup operations; logs failures but doesn't
  block execution

### Service Orchestration

Key services in `packages/cli/src/lib/services/`:

- `DeploymentOrchestrator` - High-level deployment coordination
- `ContainerDeploymentService` - Container lifecycle with zero-downtime
- `ProxyService` - kamal-proxy management (SSL termination, routing)
- `BuildService` - Image building and pushing
- `ImagePruneService` - Post-deployment cleanup

### Proxy Configuration

Services deploy to kamal-proxy as `{project}-{service}-{app_port}`.

**Single target:** One port per service **Multi-target:** Multiple ports per
service (e.g., S3 API + Admin API)

Each target's `app_port` must exist in the service's `ports` array.

### Service Runtime Options

Services support these container runtime options (in `ServiceConfiguration`):

- `command` - Override container entrypoint
- `network_mode` - Container network mode (bridge/host/none)
- `cpus`, `memory` - Resource limits
- `gpus`, `devices` - Hardware access
- `privileged`, `cap_add` - Security capabilities
- `stop_first` - Stop old container before starting new (for stateful services)

### Environment Variable Resolution

Secrets use ALL_CAPS pattern and are resolved from:

1. `.env` or `.env.{environment}` files in project root
2. Host environment (with `--host-env` flag)

Registry passwords also support this pattern (e.g., `password: GITHUB_TOKEN`).

### Naming Conventions

- **Images**: `{registry}/{project}-{service}:{version}`
- **Containers**: `{project}-{service}` (old containers get `_old_{timestamp}`
  suffix)
- **Proxy targets**: `{project}-{service}-{app_port}`
- **DNS records**: `{project}-{service}.jiji`

## Key Files

- `packages/cli/src/jiji.yml` - Authoritative configuration reference with all
  options
- `packages/cli/src/constants.ts` - System constants (ports, timeouts, defaults)
- `packages/cli/src/types/` - TypeScript interfaces for all major types
- `packages/cli/tests/mocks.ts` - Mock SSH manager for testing without real
  connections

## Testing

Tests use `MockSSHManager` from `packages/cli/tests/mocks.ts` which simulates
SSH operations via `MockServerState`. Commands are parsed and state is mutated to
simulate container operations without real SSH connections.

Pattern for adding new tests:

1. Create `new MockSSHManager("hostname")` for each host
2. Use `addMockResponse(commandPattern, response)` to stub SSH commands
3. Cast with `mock as any` when passing to functions expecting `SSHManager`

Run specific test with:

```bash
deno test --allow-all packages/cli/tests/zero_downtime_deployment_test.ts
```

## DNS Package

jiji-dns is a lightweight DNS server for service discovery. It subscribes to
Corrosion's real-time streaming API and maintains an in-memory DNS cache.

### Component Flow

```
Corrosion DB ─HTTP Stream─► CorrosionSubscriber ─► DnsCache ◄── DnsServer ◄── UDP Queries
                           (NDJSON events)        (in-memory)   (port 53)
```

### Key Components

- `packages/dns/src/corrosion_subscriber.ts` - HTTP streaming connection to
  Corrosion `/v1/subscriptions`. Auto-reconnects with exponential backoff.
- `packages/dns/src/dns_cache.ts` - In-memory cache. Newest-container-wins per
  service/server. Only healthy containers returned.
- `packages/dns/src/dns_server.ts` - UDP DNS server (RFC 1035). Routes
  `*.{serviceDomain}` to cache, forwards others to system resolvers.
- `packages/dns/src/dns_protocol.ts` - DNS packet parsing and building.

### DNS Resolution Patterns

| Pattern                                   | Example                 | Description                        |
| ----------------------------------------- | ----------------------- | ---------------------------------- |
| `{project}-{service}.{domain}`            | `casa-api.jiji`         | All healthy containers for service |
| `{project}-{service}-{instance}.{domain}` | `casa-api-primary.jiji` | Specific instance                  |

## Daemon Package

The daemon (formerly jiji-control-loop) runs continuously on each server and
handles network reconciliation:

1. Topology reconciliation - Add/remove WireGuard peers based on Corrosion state
2. Endpoint health monitoring - Check handshake times and rotate endpoints
3. Container health tracking - Validate containers and update Corrosion
4. Heartbeat updates - Keep server alive in Corrosion
5. Garbage collection - Clean up stale records
6. Public IP discovery - Periodically refresh endpoints
7. Corrosion health checks - Monitor database health
8. Split-brain detection - Detect cluster partitions

Key source files in `packages/daemon/src/`:

- `main.ts` - Entry point, main loop
- `peer_reconciler.ts` - WireGuard peer sync
- `peer_monitor.ts` - Peer health monitoring
- `container_health.ts` - Container health checks
- `corrosion_client.ts` - HTTP client for Corrosion API
- `corrosion_cli.ts` - CLI wrapper for Corrosion queries
