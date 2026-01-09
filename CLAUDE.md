# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with
code in this repository.

## Quick Start Commands

```bash
# Run CLI directly (requires all permissions)
deno task run

# Run all checks (format, lint, tests)
deno task check

# Run tests (parallel execution)
deno task test

# Run a single test file
deno test --allow-all tests/deploy_plan_test.ts

# Format and lint
deno task fmt
deno task lint

# Build compiled binary (outputs to build/jiji)
deno task build

# Build and install to /usr/local/bin/jiji
deno task install

# Version management
./bin/version          # Show current version
./bin/version 1.2.3    # Update to specific version
```

## High-Level Architecture

### Command Flow

The CLI uses Cliffy for command routing. Each command follows this pattern:

```
Command → setupCommandContext() → Load Config → Filter Hosts/Services → SSH Setup → Execute → Cleanup
```

Commands are defined in `src/main.ts` and handlers live in `src/commands/`. The
`setupCommandContext()` helper in `src/commands/command_helpers.ts` handles
common setup.

### Configuration System

Configuration classes in `src/lib/configuration/` use **lazy-loaded, cached
properties**. Validation happens at property access time, not construction. This
prevents errors for unused config sections but means errors surface later in
execution.

Hierarchy:

- `Configuration` (main) contains `SSHConfiguration`, `BuilderConfiguration`,
  `NetworkConfiguration`
- `ServiceConfiguration` (per-service) contains `BuildConfiguration`,
  `ProxyConfiguration`, `EnvironmentConfiguration`

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

Three components work together in `src/lib/network/`:

**WireGuard** (`wireguard.ts`): Encrypted mesh VPN

- Per-server /24 subnet from cluster CIDR (e.g., 10.210.0.0/16)
- IPv6 CRDT identifiers for unique peer identification

**Corrosion** (`corrosion.ts`): Distributed CRDT-based SQLite

- No central coordinator - eventual consistency
- Tables: `servers`, `containers`, `services`, `cluster_metadata`
- Gossip on port 9280, API on port 9220

**jiji-dns** (`dns.ts`): DNS-based service discovery

- Resolves `<project>-<service>.jiji` to container IPs
- Queries Corrosion for healthy containers
- Daemon-level DNS configuration (all containers auto-configured)

### SSH Connection Management

`SSHConnectionPool` in `src/utils/ssh_pool.ts` uses a semaphore for concurrency
control (default: 30 concurrent connections).

**Patterns:**

- `executeConcurrent()` - Acquire permit before SSH operation, prevents server
  overload
- `executeBestEffort()` - For cleanup operations; logs failures but doesn't
  block execution

### Service Orchestration

Key services in `src/lib/services/`:

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

## Key Files

- `src/jiji.yml` - Authoritative configuration reference with all options
- `src/constants.ts` - System constants (ports, timeouts, defaults)
- `src/types/` - TypeScript interfaces for all major types
- `tests/mocks.ts` - Mock SSH manager for testing without real connections

## Testing

Tests use mock SSH managers from `tests/mocks.ts` that simulate SSH operations.
Integration tests in `tests/` cover deployment workflows, proxy configuration,
and service filtering.

Run specific test with:

```bash
deno test --allow-all tests/zero_downtime_deployment_test.ts
```
