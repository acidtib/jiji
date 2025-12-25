# LLM.md

This file provides guidance to LLMs when working with code in this repository.

## Quick Start Commands

### Development

```bash
# Run the CLI directly (requires all permissions)
deno task run

# Run tests (parallel execution)
deno task test

# Run specific test file
deno test --allow-all tests/deploy_plan_test.ts

# Format and lint
deno task fmt
deno task lint

# Run all checks (format, lint, tests)
deno task check
```

### Building

```bash
# Build compiled binary (outputs to build/jiji)
deno task build

# Build and install to /usr/local/bin/jiji
deno task install

# Development build/install (outputs jiji_dev)
deno task dev:build
deno task dev:install
```

### Version Management

```bash
# Show current version
./bin/version

# Update to specific version (updates src/version.ts and deno.json)
./bin/version 1.2.3
```

## High-Level Architecture

### Core Design Patterns

**Command-Driven Architecture**: The CLI uses Cliffy for command routing. Each
command in `src/commands/` follows this flow:

```
Command → Load Configuration → Setup SSH Connections → Execute Services → Cleanup
```

**Lazy Configuration**: All configuration classes extend `BaseConfiguration`
with lazy-loaded, cached properties. Validation happens at access time, not
construction. This prevents errors for unused config sections but means errors
surface later in execution.

**Service Orchestration**: Multiple specialized service classes coordinate
operations:

- `DeploymentOrchestrator` - High-level deployment coordination
- `BuildService` - Image building and pushing
- `ContainerDeploymentService` - Zero-downtime container deployment
- `ProxyService` - kamal-proxy management
- `ImagePruneService` - Post-deployment cleanup

### Zero-Downtime Deployment Strategy

Critical to understand: Jiji keeps old containers running until new ones pass
health checks:

1. Rename old container (e.g., `web` → `web_old`) - keeps it running
2. Deploy new container with original name
3. Wait for health checks (proxy endpoint or container readiness)
4. Update proxy routing to new container
5. Stop and remove old container
6. Cleanup old images (keep N versions based on `retain` setting)

**Important**: If health checks fail, the old container continues serving
traffic.

### Distributed Networking Architecture

Three components work together for private networking:

**WireGuard**: Encrypted mesh VPN between servers

- Per-server subnet allocation from cluster CIDR (e.g., 10.210.0.0/16)
- Each server gets /24 subnet (254 IPs)
- IPv6 CRDT identifiers for unique peer identification

**Corrosion**: Distributed CRDT-based SQLite database

- No central coordinator - eventual consistency
- Service registry for container tracking
- Gossip protocol on port 8787, API on port 8080
- Replicated across all servers

**CoreDNS**: DNS-based service discovery

- Resolves `<service>.jiji` to container IPs
- Queries Corrosion for service locations
- Daemon-level DNS configuration (containers auto-configured)

### SSH Connection Management

**SSH Connection Pool** (`utils/ssh_pool.ts`):

- Uses custom Semaphore for concurrency limiting (default: 30 concurrent)
- Prevents server overload during parallel operations
- Pattern: `executeConcurrent()` acquires permit before SSH operation
- DNS retry logic for transient failures

**Dual SSH Implementation**:

- `node-ssh`: Primary library for command execution
- `ssh2`: Used for interactive sessions (`server exec` with `--interactive`)

### Configuration System

Configuration is hierarchical and environment-aware:

```
Configuration (main)
├── SSHConfiguration
├── BuilderConfiguration
│   └── RegistryConfiguration
├── NetworkConfiguration
├── EnvironmentConfiguration (shared across services)
└── Map<ServiceConfiguration>
    ├── BuildConfiguration
    ├── ProxyConfiguration
    │   └── HealthCheckConfiguration
    └── EnvironmentConfiguration (service-specific)
```

**Config Loading**: Searches upward from cwd for:

- `.jiji/deploy.yml` (default)
- `jiji.<environment>.yml` (with `--environment` flag)

**Important**: Config properties are immutable after creation. Use
`ConfigurationLoader.load()` to reload.

## Code Organization

### Entry Points

**`src/main.ts`**: CLI entry point using Cliffy

- Defines all top-level commands
- Global options: `--verbose`, `--quiet`, `--version`, `--config-file`,
  `--environment`, `--hosts`, `--services`
- Commands delegate to handlers in `src/commands/`

**`src/commands/`**: Command handlers

- Each command has a handler that loads config and executes operations
- Complex commands have subfolders (e.g., `services/` contains `prune.ts`,
  `restart.ts`, `logs.ts`)
- Common logic extracted to `command_helpers.ts`

### Critical Modules

**`src/lib/configuration/`**: Configuration parsing and validation

- `configuration.ts` - Main configuration class
- `service.ts` - Service-level configuration
- `validation.ts` - Schema validation
- Tests in `tests/` subdirectory

**`src/lib/services/`**: Business logic services

- `deployment_orchestrator.ts` - Orchestrates full deployment flow
- `build_service.ts` - Container image building
- `container_deployment_service.ts` - Container lifecycle management
- `proxy_service.ts` - kamal-proxy operations
- `image_prune_service.ts` - Image cleanup

**`src/lib/network/`**: Private networking components

- `setup.ts` - WireGuard, Corrosion, CoreDNS installation
- `topology.ts` - Network topology management
- `ip_discovery.ts` - Public IP detection for WireGuard

**`src/utils/`**: Cross-cutting utilities

- `ssh.ts` - SSH connection management
- `ssh_pool.ts` - Connection pooling with semaphore
- `logger.ts` - Global logging with levels
- `error_handler.ts` - Centralized error handling
- `audit.ts` - Server-side audit trail
- `mount_manager.ts` - File/directory mount handling

### Testing Structure

**Unit tests**: `src/lib/**/*_test.ts`

- Configuration validation and parsing
- Service-level logic

**Integration tests**: `tests/*_test.ts`

- `zero_downtime_deployment_test.ts` - Full deployment orchestration
- `deploy_plan_test.ts` - Deployment planning
- `proxy_service_test.ts` - Proxy configuration
- `service_filtering_test.ts` - Host/service filtering

**Mock pattern**: `tests/mocks_test.ts` provides mock SSH managers and
configurations for testing without real SSH connections.

## Important Patterns and Gotchas

### Service/Host Filtering with Wildcards

Global flags `--services` and `--hosts` support wildcard matching with `*`:

```bash
jiji deploy --services "web-*,api-*"
jiji server exec "docker ps" --hosts "server*.example.com"
```

Filtering logic centralized in `command_helpers.ts` - affects which hosts
connect and which services operate on.

### Best-Effort Cleanup Pattern

`executeBestEffort()` in SSH utilities: Command failures logged but don't block
execution. Used for cleanup operations (removing old containers, stopping
services) to prevent partial failures from blocking completion.

**Critical**: Never use for operations that must succeed (image pulls, container
creation).

### Environment Variable Type Conversion

Non-string types (numbers, booleans) in environment config automatically
converted to strings for container compatibility:

```yaml
environment:
  clear:
    PORT: 3000 # → "3000"
    DEBUG: true # → "true"
```

### Deployment Locks

Distributed lock management prevents concurrent deployments:

- Stored in `.jiji/locks/` on primary server
- Acquired before deployment, released after
- Check with `jiji lock status`

### Registry Auto-Detection

Automatic namespace detection for supported registries:

- **GHCR** (`ghcr.io`): `username/project-name`
- **Docker Hub** (`docker.io`): `username`
- **Local registries**: No namespace required

### Image Retention

After each deployment, old images are automatically pruned:

- Keep N recent versions (default: 3, configurable per-service with `retain`)
- Sorted by creation time
- Running containers never removed
- Dangling (untagged) images also cleaned

## Key Files for Reference

**`src/jiji.yml`**: Authoritative configuration reference with all available
options and detailed comments

**`docs/architecture.md`**: Detailed system architecture diagrams and component
explanations

**`docs/configuration-reference.md`**: Configuration guide with examples

**`docs/network-reference.md`**: Private networking setup and troubleshooting

**`docs/deployment-guide.md`**: Advanced deployment patterns, CI/CD integration,
rollback procedures

## Development Notes

### Required Permissions

The CLI requires extensive permissions due to SSH, networking, and filesystem
operations:

```bash
--allow-read --allow-write --allow-net --allow-run --allow-ffi --allow-env --allow-sys
```

Or use `--allow-all` for development (as in `deno task run`).

### Adding New Commands

1. Create handler in `src/commands/` (or subfolder for subcommands)
2. Register in `src/main.ts` using Cliffy's `.command()` API
3. Follow existing patterns for config loading and SSH setup
4. Use `command_helpers.ts` utilities for common operations
5. Add tests in `tests/` directory

### Modifying Configuration Schema

1. Update types in `src/types/`
2. Update configuration classes in `src/lib/configuration/`
3. Add validation in `src/lib/configuration/validation.ts`
4. Update reference config in `src/jiji.yml`
5. Add tests in `src/lib/configuration/tests/`

### Working with SSH Operations

Always use the SSH pool for concurrent operations:

```typescript
await sshManager.executeConcurrent(
  hosts,
  async (ssh, host) => {
    // Your SSH command here
  },
);
```

For sequential operations on single host:

```typescript
const ssh = await sshManager.getConnection(host);
const result = await ssh.exec(command);
```

### Logging Best Practices

Use appropriate log levels:

- `logger.debug()` - Verbose diagnostic info (only shown with `--verbose`)
- `logger.info()` - Normal operation progress
- `logger.warn()` - Non-fatal issues
- `logger.error()` - Errors that don't stop execution
- `logger.success()` - Successful completion

### Error Handling

Use `handleCommandError()` from `error_handler.ts` at command boundaries:

```typescript
try {
  // command logic
} catch (error) {
  handleCommandError(error);
}
```

This ensures consistent error formatting and audit logging.
