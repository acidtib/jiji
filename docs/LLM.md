# LLM.md

This file provides guidance to LLM's when working with code in this repository.

## Project Overview

Jiji is an infrastructure management tool for deploying containerized applications across multiple servers. It provides service deployment, private networking with WireGuard/DNS, registry management, and deployment orchestration with zero-downtime rollouts.

**Tech Stack**: Deno 2.5+, TypeScript, Cliffy CLI framework, SSH2/node-ssh for remote execution

## Development Commands

```bash
# Run locally during development
deno task run

# Build compiled binary
deno task build

# Build and install to /usr/local/bin/jiji
deno task install

# Development build/install (outputs jiji_dev)
deno task dev:build
deno task dev:install

# Testing and code quality
deno task test           # Run all tests
deno task fmt            # Format code
deno task lint           # Lint code
deno task check          # Run fmt check, lint, and tests

# Run specific test file
deno test --allow-all tests/deploy_plan_test.ts

# Bump version across codebase and docs
./bin/version <new-version>
```

## Architecture

### Configuration System (`src/lib/configuration/`)

The configuration system uses a class-based hierarchy for type-safe YAML config parsing:

- **`Configuration`** - Main entry point orchestrating all config aspects (project, services, SSH, network, builder, environment)
- **`ServiceConfiguration`** - Individual service config (image/build, hosts, ports, volumes, proxy, env vars)
- **`SSHConfiguration`** - SSH connection settings with support for proxies, keys, and .ssh/config parsing
- **`NetworkConfiguration`** - WireGuard mesh networking and DNS service discovery settings
- **`BuilderConfiguration`** - Local or remote build configuration
- **`EnvironmentConfiguration`** - Shared environment variables across services

All configs extend `BaseConfiguration` which provides validation helpers. The system uses lazy loading and caching for performance.

### Service Deployment (`src/lib/services/`)

**Deployment flow** (orchestrated by `DeploymentOrchestrator`):

1. **Proxy Installation** - Install kamal-proxy on hosts if services use proxy
2. **Container Deployment** - Zero-downtime rollout with health checks
3. **Proxy Configuration** - Route traffic to new containers
4. **Old Container Cleanup** - Remove previous versions after successful deployment

Key services:
- **`DeploymentOrchestrator`** - Main deployment workflow coordinator
- **`ContainerDeploymentService`** - Zero-downtime container rollout (keeps old running until new is healthy)
- **`ProxyService`** - kamal-proxy installation and configuration
- **`BuildService`** - Image building (local or remote)
- **`ImagePushService`** - Push images to registries
- **`ImagePruneService`** - Clean up old images (keeps last N versions)
- **`RegistryAuthService`** - Registry authentication management

### Private Networking (`src/lib/network/`)

Creates WireGuard mesh VPN with automatic DNS-based service discovery:

- **`setup.ts`** - Main orchestrator for network initialization
- **`wireguard.ts`** - WireGuard interface management, key generation, config writing
- **`corrosion.ts`** - Distributed key-value store for cluster state (built on CRDT)
- **`dns.ts`** - CoreDNS setup for service discovery (containers resolve `service.jiji`)
- **`topology.ts`** - Network topology management (server discovery, peer relationships)
- **`subnet_allocator.ts`** - IPv4 subnet allocation (/24 subnets) for WireGuard interfaces
- **`peer_monitor.ts`** - Monitor peer connectivity and update configurations
- **`control_loop.ts`** - Continuous reconciliation of network state

Network flow:
1. Each server gets unique WireGuard keys and IPv4 subnet (/24 from cluster CIDR)
2. Corrosion syncs cluster metadata across servers
3. CoreDNS provides DNS resolution for service names
4. Control loop maintains peer connections and updates configs

### SSH Management (`src/utils/`)

- **`ssh.ts`** - Main `SSHManager` class for remote command execution
- **`ssh_pool.ts`** - Connection pooling with LRU eviction
- **`ssh_proxy.ts`** - ProxyJump and ProxyCommand support
- **`ssh_config_parser.ts`** - Parse ~/.ssh/config for connection settings

SSH connections support:
- SSH agent authentication
- Private key files
- ProxyJump/ProxyCommand
- Connection reuse via pooling
- Parallel execution across multiple hosts

### Command Structure (`src/commands/`)

Commands are organized by domain:
- **`init.ts`** - Initialize `.jiji/deploy.yml` configuration stub
- **`build.ts`** - Build container images
- **`deploy.ts`** - Deploy services (with deployment plan confirmation)
- **`remove.ts`** - Remove services and cleanup
- **`services/`** - Service management (restart, prune)
- **`server/`** - Server operations (init, exec, teardown)
- **`registry/`** - Registry management (setup, login, logout, remove)
- **`network.ts`** - Network operations (status, teardown)
- **`audit.ts`** - View audit logs from servers
- **`lock.ts`** - Deployment lock management
- **`version.ts`** - Show application version

### Utilities (`src/utils/`)

- **`logger.ts`** - Structured logging with log levels
- **`config.ts`** - Config file loading with environment support
- **`error_handling.ts`** - Custom error types and error handling
- **`audit.ts`** - Server-side audit trail logging
- **`lock.ts`** - Distributed deployment locking
- **`git.ts`** - Git SHA extraction for image tagging
- **`version_manager.ts`** - Image version tracking and management
- **`mount_manager.ts`** - Volume/file/directory mount handling
- **`registry_manager.ts`** - Registry URL parsing and namespace detection (auto-detects GHCR, Docker Hub)
- **`service_filter.ts`** - Wildcard filtering for hosts/services
- **`engine.ts`** - Container engine abstraction (Docker/Podman)

## Configuration

Main config file: `.jiji/deploy.yml` (or `jiji.<environment>.yml` with `--environment` flag)

The reference configuration with all options is in `src/jiji.yml` - this is the authoritative source for config structure.

### Global Options

All commands support:
- `--verbose` - Debug logging
- `--version` - Specify app version (overrides git SHA)
- `--config-file` - Custom config path
- `--environment` - Environment name for config
- `--hosts` - Filter hosts (supports wildcards with `*`)
- `--services` - Filter services (supports wildcards with `*`)

## Code Patterns

### Configuration Access

```typescript
// Load configuration
const config = await loadConfig(configFile, environment);

// Access typed properties
const projectName = config.project;
const services = config.services; // Map<string, ServiceConfiguration>
const ssh = config.ssh;
const network = config.network;

// Iterate services
for (const [name, service] of config.services) {
  const hosts = service.hosts;
  const image = service.image;
}
```

### SSH Execution

```typescript
// Create SSH manager
const ssh = new SSHManager(config.ssh.toConnectionConfig(host), host);

// Execute command
const result = await ssh.execute("docker ps", { timeout: 30000 });

// Execute with best-effort (doesn't throw)
await ssh.executeBestEffort("systemctl restart service");

// Cleanup
await ssh.dispose();
```

### Service Deployment

```typescript
// Orchestrate deployment
const orchestrator = new DeploymentOrchestrator(config);
const result = await orchestrator.orchestrate(
  servicesToDeploy,
  { version: "v1.2.3" }
);

// Check results
if (result.success) {
  console.log(result.metrics);
}
```

### Error Handling

Use custom error types from `utils/error_handling.ts`:
- `ConfigurationError` - Config validation errors
- `RegistryError` - Registry operation failures
- `SSHError` - SSH connection/execution errors
- `DeploymentError` - Deployment failures

## Testing

Tests use Deno's built-in test framework. Located in:
- `tests/` - Integration and end-to-end tests
- `src/lib/configuration/tests/` - Configuration system tests
- `src/utils/tests/` - Utility function tests

Test patterns:
- Use `Deno.test()` with descriptive names
- Mock SSH connections for deployment tests (see `tests/mocks.ts`)
- Test configuration validation separately from loading
- Integration tests verify full workflows

## Important Implementation Details

### Zero-Downtime Deployments

The deployment system keeps old containers running until new ones pass health checks:

1. Deploy new container with version tag
2. Wait for health check to pass (via proxy health endpoint or container readiness)
3. Configure proxy to route to new container
4. Stop and remove old container
5. Clean up old images (keeping last N versions)

### Registry Auto-Detection

`registry_manager.ts` automatically detects registry namespaces:
- **GHCR** (`ghcr.io`) → `username/project-name`
- **Docker Hub** (`docker.io`) → `username`
- **Local/other** → No namespace

### Image Tagging Strategy

Images are tagged with:
1. Git SHA (default) - `registry/project/service:abc1234`
2. Custom version (via `--version`) - `registry/project/service:v1.2.3`

The `version_manager.ts` tracks deployed versions per service/host for rollback and pruning.

### Private Network Architecture

The private network uses IPv4 addressing (10.210.0.0/16 by default) for WireGuard tunnels, with IPv6 management addresses (fdcc::/16) for Corrosion gossip protocol. Each server:
1. Generates unique WireGuard keypair
2. Gets allocated /24 IPv4 subnet from allocator (e.g., 10.210.0.0/24, 10.210.1.0/24)
3. Derives deterministic IPv6 management address from public key for Corrosion
3. Registers in Corrosion distributed store
4. Establishes WireGuard peers to all other servers
5. Runs CoreDNS for service.jiji DNS resolution
6. Container engine configured to use CoreDNS as resolver

The control loop continuously reconciles network state by monitoring Corrosion for topology changes.

### Environment Variable Handling

Environment variables from non-string types (numbers, booleans) are automatically converted to strings during deployment. This is handled in the service configuration layer to ensure container compatibility.
