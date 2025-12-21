# LLM.md

This file provides guidance to LLM's when working with code in this repository.

## Project Overview

Jiji is a container orchestration tool for deploying containerized applications across multiple servers without vendor lock-in. It provides service management, private networking (WireGuard mesh), deployment locks, and proxy integration.

Built with Deno and TypeScript, targeting Linux/MacOS/Windows environments.

## Development Commands

### Running and Testing

```bash
# Run the CLI locally
deno task run

# Run with specific command
deno task run deploy --help

# Run tests
deno task test
# or
deno test --allow-all

# Format code
deno task fmt

# Lint code
deno task lint

# Run all checks (format, lint, test)
deno task check
```

### Building and Installing

```bash
# Build development binary
deno task dev:build

# Install development binary to /usr/local/bin/jiji_dev
deno task dev:install

# Build production binary
deno task build

# Install production binary to /usr/local/bin/jiji
deno task install
```

### Version Management

```bash
# Update version across all files
./bin/version <new-version>
```

## Architecture

### Command Structure

Commands follow a hierarchical pattern using Cliffy:
- **Top-level commands** in `src/commands/`: `init`, `build`, `deploy`, `remove`, `version`, `audit`, `lock`
- **Nested commands** in subdirectories: `server/`, `registry/`, `network/`
- **Main entry** at `src/main.ts` sets up global options and command registration

Global options are available on all commands:
- `--verbose`: Enable debug logging
- `--version=<VERSION>`: Specify app version for deployments
- `--config-file=<PATH>`: Custom config file path
- `--environment=<ENV>`: Use environment-specific config (e.g., `jiji.staging.yml`)
- `--hosts=<HOSTS>`: Target specific hosts (comma-separated, supports wildcards)
- `--services=<SERVICES>`: Target specific services (comma-separated, supports wildcards)

### Configuration System

The configuration system is modular and type-safe, located in `src/lib/configuration/`:

- **`Configuration`** (main orchestrator): Loads and validates the entire config
- **`ConfigurationLoader`**: Handles YAML file discovery and loading
- **`BaseConfiguration`**: Base class with common validation methods
- **Specialized configs**: `SSHConfiguration`, `ServiceConfiguration`, `BuilderConfiguration`, `NetworkConfiguration`, `RegistryConfiguration`, `ProxyConfiguration`, `EnvironmentConfiguration`
- **Validation**: `ConfigurationValidator` with preset validators and rules

Config files are discovered in order:
1. Explicit `--config-file` path
2. Environment-specific: `.jiji/<project>.<environment>.yml`
3. Default: `.jiji/deploy.yml`

### Service Layer Pattern

Core business logic is extracted into service classes in `src/lib/services/`:

- **`BuildService`**: Handles image building for services with build configs
- **`ContainerDeploymentService`**: Deploys containers to remote hosts
- **`ContainerRegistryService`**: Manages container registry operations
- **`ProxyService`**: Manages kamal-proxy installation and configuration
- **`RegistryAuthService`**: Handles registry authentication
- **`ImagePushService`**: Pushes images to registries

Services are instantiated in commands and called with appropriate parameters.

### Command Helper Pattern

Common command patterns are consolidated in `src/utils/command_helpers.ts`:

- **`setupCommandContext()`**: Loads config, filters hosts/services, establishes SSH connections
- **`withCommandContext()`**: Wrapper that sets up context, executes handler, handles errors, cleans up
- **`cleanupSSHConnections()`**: Disposes SSH managers
- **`resolveTargetHosts()`**: Lightweight host resolution without SSH

Typical command structure:
```typescript
export const myCommand = new Command()
  .description("My command")
  .action(async (options) => {
    const globalOptions = options as unknown as GlobalOptions;
    let ctx: Awaited<ReturnType<typeof setupCommandContext>> | undefined;
    
    try {
      await log.group("Operation Name", async () => {
        ctx = await setupCommandContext(globalOptions);
        const { config, sshManagers, targetHosts } = ctx;
        
        // Command logic here
      });
    } catch (error) {
      await handleCommandError(error, {
        operation: "Operation Name",
        component: "component-id",
        sshManagers: ctx?.sshManagers,
        projectName: ctx?.config?.project,
        targetHosts: ctx?.targetHosts,
      });
    } finally {
      if (ctx?.sshManagers) {
        cleanupSSHConnections(ctx.sshManagers);
      }
    }
  });
```

### SSH Connection Management

SSH functionality is in `src/utils/ssh.ts`:

- **`SSHManager`**: Wraps `node-ssh` with additional utilities (execute, file upload/download, disposal)
- **`setupSSHConnections()`**: Establishes connections to multiple hosts with retry logic and partial connection support
- Always dispose of SSH connections in `finally` blocks

### Private Networking (WireGuard)

The network subsystem (`src/lib/network/`) provides mesh VPN and service discovery:

- **`wireguard.ts`**: WireGuard config generation and keypair management
- **`topology.ts`**: Determines mesh network topology and peer relationships
- **`dns.ts`**: CoreDNS configuration for service discovery (e.g., `api.jiji`, `postgres.jiji`)
- **`corrosion.ts`**: Distributed key-value store for network state
- **`routes.ts`**: Container network routing configuration
- **`control_loop.ts`**: Network state reconciliation
- **`ip_discovery.ts`**: Public IP detection for WireGuard endpoints

Network state is stored in `src/lib/network/stores/`.

### Error Handling and Audit Trail

- **`error_handler.ts`**: Centralized error handling with audit logging
- **`audit.ts`**: Server-side audit trail logging (stored in `.jiji/audit.txt` on remote hosts)
- All operations are logged with timestamps, action types, status, and context

### Logging

Structured logging via `src/utils/logger.ts`:
- Log levels: `debug`, `info`, `status`, `warn`, `error`, `success`
- Log groups with `log.group(title, async () => {...})`
- Component tags for filtering: `log.info("message", "component")`
- Set level with `setGlobalLogLevel("debug")` (controlled by `--verbose`)

### Registry System

Registry management (`src/lib/registry_service.ts`, `src/utils/registry_manager.ts`, `src/utils/registry_config.ts`):

- Supports local registries (started via Podman/Docker with SSH port forwarding)
- Supports remote registries (Docker Hub, GHCR, ECR, custom)
- Auto-detection of namespace requirements for GHCR (`username/project`) and Docker Hub (`username`)
- Registry auth handled via `RegistryAuthService`

### Deployment Lock System

Distributed deployment locks prevent concurrent deployments:
- Locks stored in `.jiji/locks/<project>/deploy.lock` on remote hosts
- Commands: `jiji lock acquire`, `jiji lock release`, `jiji lock status`, `jiji lock show`
- Lock acquisition requires majority consensus across hosts

### Proxy Integration

Built-in kamal-proxy support for HTTP routing:
- `ProxyService` installs and manages kamal-proxy on hosts
- Service-level proxy config with host, SSL, health checks
- Automatic route configuration for proxy-enabled services

## Key Patterns and Conventions

### Type Safety

- Global types in `src/types.ts` and `src/types/*.ts`
- Command options always cast to `GlobalOptions` interface
- Configuration classes use TypeScript strict mode

### Utility Organization

Utilities in `src/utils/`:
- `config.ts`: Service filtering and config helpers
- `engine.ts`: Container engine detection and command building
- `git.ts`: Git SHA and version detection
- `lock.ts`: Distributed lock implementation
- `mount_manager.ts`: Volume/bind mount handling
- `port_forward.ts`: SSH reverse port forwarding (for local registry)
- `promise_helpers.ts`: Promise utilities (parallel execution, retry logic)
- `proxy.ts`: Kamal-proxy utilities
- `registry_*.ts`: Registry-related utilities
- `service_filter.ts`: Service matching and filtering with wildcard support
- `version_manager.ts`: Version tag determination (git SHA or custom)

### Testing

Tests are colocated with source in `tests/` subdirectories:
- `src/lib/configuration/tests/`
- `src/utils/tests/`

Use Deno's built-in test framework with `--allow-all` permissions.

### Configuration Validation

All configuration is validated on load:
- Required fields throw `ConfigurationError` if missing
- Type validation for strings, numbers, arrays, objects
- Cross-field validation (e.g., host consistency)
- Warnings for suboptimal configs (e.g., too many hosts)

### Service Filtering

Services and hosts support wildcard patterns:
- `--services "web*"` matches `web`, `web-api`, `web-frontend`
- `--hosts "server*.example.com"` matches all servers with that pattern
- Implemented in `matchServicePattern()` and used throughout commands

## Important Implementation Details

### Container Engine Abstraction

Both Docker and Podman are supported via `src/utils/engine.ts`:
- Use `buildDockerCommand()` or `buildPodmanCommand()` to construct engine-specific commands
- Engine is specified in `builder.engine` config
- Some commands differ between engines (e.g., `podman pod` vs `docker network`)

### File Transfers

SSH file operations via `SSHManager`:
- Use `uploadFile()` for single files
- Use `uploadDirectory()` for directories
- Files are transferred before container creation for mounts

### Parallel Execution

Use `executeInParallel()` from `src/utils/promise_helpers.ts`:
- Executes operations across multiple hosts concurrently
- Configurable concurrency limits
- Aggregates results and handles errors

### Version Tagging

Images are tagged with version identifiers:
- Default: git SHA (short form, 7 chars)
- Override with `--version` flag
- Managed by `VersionManager.determineVersionTag()`

### Configuration File Search

`ConfigurationLoader` searches upward from current directory to find config files, stopping at git repo root or filesystem root.

## Working with This Codebase

### Adding a New Command

1. Create command file in `src/commands/` (or subdirectory for nested commands)
2. Use `setupCommandContext()` for consistent initialization
3. Follow the error handling pattern with `handleCommandError()`
4. Register in `src/main.ts` or parent command index
5. Add tests in colocated `tests/` directory

### Adding a New Service

1. Create service class in `src/lib/services/`
2. Inject dependencies (engine, config, SSH managers) via constructor
3. Use `log.group()` for operation grouping
4. Handle errors and throw meaningful exceptions
5. Use service in command handlers

### Modifying Configuration Schema

1. Update types in `src/lib/configuration/`
2. Add validation rules in `validation.ts`
3. Update `src/jiji.yml` example with new fields and documentation
4. Test with `Configuration.validateFile()`

### Adding Network Features

1. Network logic goes in `src/lib/network/`
2. State management uses stores in `src/lib/network/stores/`
3. DNS changes require CoreDNS config updates
4. Test mesh connectivity across multiple hosts
