# LLM.md

This file provides comprehensive guidance for AI assistants (Claude, Qwen, and
others) when working with code in this repository.

## Project Overview

Jiji is a container deployment tool for managing containerized applications
across multiple servers. It provides infrastructure-as-code capabilities without
vendor lock-in, supporting both Docker and Podman as container engines.

**Current Status**: Under heavy development, not production-ready.

**Key Features:**

- Service management (build, deploy, remove containerized services)
- Server bootstrap capabilities with support for Podman and Docker
- Remote command execution across multiple servers
- Configuration management via YAML configuration files
- Server-side audit trail with comprehensive logging of operations
- Registry management (local and remote)
- Built-in support for kamal-proxy for traffic routing
- Mount management for files, directories, and volumes

## Development Environment

### Prerequisites

- [Deno](https://deno.land/) 2.5.6+
- Node.js 24.11.1+ (for CI/CD)
- npm 11.5.1+ (for trusted publishing)

### Development Commands

#### Running the CLI

```bash
# Run with all permissions
deno task run

# Run specific commands
deno run --allow-all src/main.ts <command>

# Direct execution with explicit permissions
deno run --allow-read --allow-write --allow-net --allow-run --allow-ffi --allow-env --allow-sys src/main.ts
```

#### Testing & Quality

```bash
# Run all tests
deno test --allow-all
# OR
deno task test

# Format code
deno fmt

# Lint code
deno lint

# Run all checks (format, lint, test)
deno task check
```

#### Building & Installation

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

#### Version Management

```bash
# Update version using the version script
./bin/version
```

## Architecture

### Project Structure

- **Main entry point** (`src/main.ts`): Sets up the CLI with various commands
- **Commands** (`src/commands/`): Different functionalities organized by command
  type
- **Library modules** (`src/lib/`): Core business logic and configuration
  management
- **Utilities** (`src/utils/`): Helper functions for logging, SSH, audit trails,
  etc.
- **Types** (`src/types.ts`): Shared TypeScript interfaces and types

### Core Components

**Configuration System** (`src/lib/configuration/`)

- Multi-layered configuration architecture with lazy loading
- `Configuration` class orchestrates all configuration aspects (project, SSH,
  services, environment, builder)
- Each configuration aspect has its own specialized class:
  - `SSHConfiguration`: SSH connection settings and key management
  - `ServiceConfiguration`: Individual service definitions (image/build, hosts,
    ports, mounts, proxy)
  - `EnvironmentConfiguration`: Environment variables with secret/clear
    separation
  - `BuilderConfiguration`: Container engine and registry settings
  - `RegistryConfiguration`: Container registry configuration (local/remote)
  - `ProxyConfiguration`: Proxy routing and health check settings
- `ConfigurationLoader`: Handles file system loading with environment support
- Validation system with `ConfigurationValidator` and preset validators
- Supports environment-specific configs (e.g., `.jiji/deploy.yml` or
  `jiji.staging.yml`)

**Command Layer** (`src/commands/`)

- Built with Cliffy framework for CLI functionality
- Each command is a separate file/directory:
  - `init.ts`: Initialize configuration stub
  - `build.ts`: Build container images
  - `deploy.ts`: Deploy services to servers
  - `remove.ts`: Remove services and cleanup
  - `server/`: Server management (bootstrap, exec)
  - `registry/`: Registry management (setup, login, logout, remove)
  - `audit.ts`: View server-side audit logs
  - `lock.ts`: Manage deployment locks
- Global options defined in `main.ts` include: verbose, version, config-file,
  environment, hosts, services

**Service Layer** (`src/lib/services/`)

- `BuildService`: Handles container image building with support for multi-arch
  builds
- `ContainerRunBuilder`: Constructs container run commands with all options
- `ImagePushService`: Manages image pushing to registries

**SSH & Remote Execution** (`src/utils/ssh.ts`)

- `SSHManager`: Manages SSH connections using node-ssh and ssh2
- Supports SSH config files, ProxyJump, and ProxyCommand
- `SSHPool`: Connection pooling for multiple hosts
- `SSHProxy`: Handles SSH proxy connections
- Audit logging for all SSH operations

**Registry System**

- `RegistryService` (`src/lib/registry_service.ts`): High-level registry
  operations
- `RegistryAuthenticator` (`src/lib/registry_authenticator.ts`): Auto-detects
  registry type and handles authentication
- `RegistryManager` (`src/utils/registry_manager.ts`): Manages local and remote
  registries
- `RegistryConfigManager` (`src/utils/registry_config.ts`): Persists registry
  credentials in `.jiji/registry.json`
- Supports both local (self-hosted) and remote (Docker Hub, GitHub Container
  Registry, etc.) registries

**Proxy Integration** (`src/utils/proxy.ts`)

- `ProxyCommands`: Manages kamal-proxy for routing traffic to services
- Supports health checks, SSL configuration, and dynamic routing

**Utilities** (`src/utils/`)

- `logger.ts`: Structured logging with groups, levels, and context
- `audit.ts`: Server-side audit trail functionality
- `engine.ts`: Container engine abstraction (Docker/Podman)
- `mount_manager.ts`: File, directory, and volume mount preparation
- `port_forward.ts`: SSH port forwarding for local registry access
- `lock.ts`: Deployment locking mechanism
- `git.ts`: Git operations for version tracking
- `service_filter.ts`: Service filtering with wildcard support
- `error_handling.ts`: Centralized error handling with typed error codes

### Key Patterns

1. **Configuration Loading**: Configuration is loaded via
   `Configuration.load()`, which:
   - Searches for config files (environment-specific or default)
   - Validates the configuration
   - Returns a fully-typed Configuration object with lazy-loaded
     sub-configurations

2. **SSH Connection Management**: Most commands follow this pattern:
   - Load configuration
   - Filter services/hosts if needed
   - Create SSH connections using `setupSSHConnections()`
   - Execute operations via `sshManager.execCommand()`
   - Close connections in finally block

3. **Service Deployment Flow**:
   - Load configuration and filter services
   - Optionally build images (BuildService)
   - Set up port forwarding for local registry access
   - Prepare mounts (files, directories, volumes)
   - Configure proxy if enabled
   - Build container run command (ContainerRunBuilder)
   - Execute deployment via SSH
   - Audit all operations

4. **Error Handling**: Use typed error codes from `RegistryErrorCodes` and
   `createRegistryError()` for consistent error messages

5. **Audit Trail**: All operations are logged server-side to `.jiji/audit.txt`
   on target servers using `createServerAuditLogger()`

## Configuration File Structure

The `.jiji/deploy.yml` file defines:

- `project`: Project name for service organization
- `ssh`: SSH connection settings (user, port, keys, proxy)
- `builder`: Container engine (docker/podman), registry settings
- `environment`: Shared environment variables across services
- `services`: Map of service definitions, each containing:
  - `image` OR `build`: Container image or build configuration
  - `hosts`: Target servers for deployment
  - `ports`: Port mappings
  - `volumes`: Volume mounts
  - `files`: File mounts (string or hash format with mode/owner)
  - `directories`: Directory mounts
  - `environment`: Service-specific environment variables
  - `proxy`: Proxy configuration (host, SSL, health checks)
  - `command`: Override container command

## Development Conventions

### Permissions Model

Jiji requires various Deno permissions:

- `--allow-read` / `--allow-write`: File system access
- `--allow-net`: Network communication
- `--allow-run`: Execute subprocesses (Docker/Podman commands)
- `--allow-ffi`: Foreign function interface (if needed)
- `--allow-env`: Access environment variables
- `--allow-sys`: System information access

### Logging

The project uses a structured logging system (`src/utils/logger.ts`) with
different log levels (info, success, warn, error, debug) and supports grouped
log output for complex operations.

### Error Handling

Comprehensive error handling with custom error types and validation systems for
configuration files. Use typed error codes from `RegistryErrorCodes` and
`createRegistryError()` for consistent error messages.

### Testing Approach

- Tests are located in `src/utils/tests/` and use Deno's built-in test framework
- Run with `deno test --allow-all` as tests require file system, network, and
  subprocess permissions
- Tests follow the pattern of importing from `@std/assert` and using
  `Deno.test()`
- Use mock objects for testing SSH operations and other external dependencies

## Important Notes

### Core Functionality

- **Service Filtering**: Commands support `--services` and `--hosts` flags with
  wildcard patterns (e.g., `web*`)
- **Multi-Architecture**: BuildService supports building for different
  architectures (amd64, arm64)
- **Registry Types**: Auto-detected from URL format (Docker Hub, GitHub
  Container Registry, generic registries)
- **SSH Configuration**: Automatically parses `~/.ssh/config` for host
  configurations
- **Port Forwarding**: Local registry access uses SSH port forwarding when
  needed
- **Audit Logs**: Operations are logged on each target server (not client-side),
  accessible via `jiji audit`
- **Mount Formats**: Supports both shorthand string format
  (`local:remote:options`) and hash format with detailed configuration
- **Proxy Integration**: Built-in support for kamal-proxy with automatic route
  configuration

### Security Considerations

- SSH connections use key-based authentication
- Registry credentials can be managed securely via environment variables or
  `.jiji/registry.json`
- Container operations are performed with appropriate security contexts

### Version Management

The project includes a version management script (`./bin/version`) that handles
version updates consistently across:

- `src/version.ts` (source code constant)
- `deno.json` (Deno configuration)
- `README.md` (installation examples)

## CI/CD

GitHub Actions workflow (`.github/workflows/ci.yml`):

- Runs on PRs to main/develop branches
- Checks formatting, linting, and tests
- Uses Deno 2.5.6+ and Node.js 24.11.1+
- Requires npm 11.5.1+ for trusted publishing
- Caches Deno dependencies
- Installs native dependencies with `--allow-scripts` for cpu-features and ssh2

## Publishing

Published to both NPM and JSR as `@jiji/cli`. Installation script available at
`https://get.jiji.run/install.sh` for Linux/MacOS.

## Available Commands

### Core Commands

- `init`: Create configuration stub in `.jiji/deploy.yml`
- `build`: Build container images for services
- `deploy`: Deploy services to remote servers
- `remove`: Remove services and clean up
- `server bootstrap`: Bootstrap servers with container engine
- `server exec`: Execute remote commands on servers
- `registry setup/login/logout/remove`: Manage container registries
- `audit`: View server-side audit logs
- `lock`: Handle configuration locking
- `version`: Display version information

### Global Options

Available across most commands:

- `--verbose`: Enable verbose output
- `--version`: Show version information
- `--config-file`: Specify custom configuration file
- `--environment`: Target specific environment
- `--hosts`: Filter by specific hosts (supports wildcards)
- `--services`: Filter by specific services (supports wildcards)

## Project Status

This project is under heavy development and not yet production-ready. When
working with this codebase, be aware that APIs and architecture may change
frequently.
