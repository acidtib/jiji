# Jiji

> **WIP**: Under heavy development, not production ready.

Deploy containerized apps across servers with simplicity, speed, and portability. No infrastructure vendor lock-in required.

## Features

**Service Management**: Build, deploy, and remove containerized services across multiple servers

**Server Bootstrap**: Bootstrap servers with curl and Podman or Docker

**Private Networking**: WireGuard mesh VPN with automatic service discovery via DNS for secure container-to-container communication across the cluster

**Deployment Locks**: Prevent concurrent deployments with distributed lock management

**Remote Command Execution**: Execute custom commands across multiple servers

**Configuration Management**: Create and manage infrastructure configurations

**Server-Side Audit Trail**: Comprehensive logging of all operations directly on target servers

**Registry Management**: Manage container registries (local and remote) with automatic namespace detection for GHCR and Docker Hub

**Proxy Integration**: Built-in support for kamal-proxy for routing traffic to services

**Mount Management**: Support for file, directory, and volume mounts

**CLI Interface**: Easy-to-use command-line interface built with Cliffy

## Installation

### From NPM

```bash
npm install -g jiji
```

### From JSR

```bash
deno install --allow-all --name jiji jsr:@jiji/cli
```

### Linux/MacOS

```bash
curl -fsSL https://get.jiji.run/install.sh | sh
```

You can also install a specific version by setting the VERSION environment
variable:

```bash
curl -fsSL https://get.jiji.run/install.sh | VERSION=v0.1.8 sh
```

### Windows

Download the latest Windows binary from the [releases page](https://github.com/acidtib/jiji/releases) and add it to your PATH:

1. Download `jiji-windows-x86_64.exe` from the latest release
2. Rename it to `jiji.exe`
3. Place it in a directory that's in your PATH (e.g., `C:\Windows\System32` or create a dedicated folder and add it to PATH)

Or use PowerShell to download and install:

```powershell
# Download to current directory
Invoke-WebRequest -Uri "https://github.com/acidtib/jiji/releases/download/v0.1.8/jiji-windows-x86_64.exe" -OutFile "jiji.exe"
```

## Usage

### Initialize Configuration

Create a configuration stub in `.jiji/deploy.yml`:

```bash
jiji init
```

### Build Images

Build container images for your services (with optional push to registry):

```bash
# Build all services defined with build configuration
jiji build

# Build without pushing to registry
jiji build --no-push

# Build without using cache
jiji build --no-cache

# Build specific services only
jiji build --services "web,api"
```

### Deploy Services

Deploy services to remote servers with full lifecycle management:

```bash
# Deploy all services
jiji deploy

# Build and deploy services in one command
jiji deploy --build

# Deploy without using cache (requires --build)
jiji deploy --build --no-cache

# Deploy specific services only
jiji deploy --services "web,api"

# Deploy using specific version tag (instead of git SHA)
jiji deploy --version v1.2.3

# Deploy to specific hosts only
jiji deploy --hosts "server1.example.com,server2.example.com"
```

### Remove Services

Remove services and clean up project directories:

```bash
# Remove all services (prompts for confirmation)
jiji remove

# Remove without confirmation prompt
jiji remove --confirmed

# Remove specific services only
jiji remove --services "web,api"
```

### Registry Management

Manage container registries for storing and retrieving images with automatic configuration:

```bash
# Setup local or remote registry
jiji registry setup

# Skip local or remote setup
jiji registry setup --skip-local
jiji registry setup --skip-remote

# Login to a remote registry
jiji registry login

# Logout from a registry
jiji registry logout

# Remove local registry
jiji registry remove
```

**Auto-Detection Support**: Jiji automatically detects namespace requirements for supported registries:
- **GHCR** (`ghcr.io`): Auto-namespace as `username/project-name`
- **Docker Hub** (`docker.io`): Auto-namespace as `username`
- **Local registries**: No namespace required

See [Registry Auto-Detection](docs/registry-auto-detection.md) for detailed configuration examples.

### Server Management

Bootstrap servers with container runtime:

```bash
jiji server bootstrap
```

Execute custom commands on remote hosts:

```bash
# Execute a command on all configured hosts
jiji server exec "docker ps"

# Execute on specific hosts only
jiji server exec "systemctl status docker" --hosts "server1.example.com,server2.example.com"

# Execute in parallel across all hosts
jiji server exec "df -h" --parallel

# Run commands interactively (for console/bash sessions)
jiji server exec "bash" --interactive --hosts "server1.example.com"

# Interactive sessions support multiple ways to disconnect:
# - Press Ctrl+C, Ctrl+D, or Ctrl+\ to terminate
# - Type 'exit' and press Enter
# - Sessions auto-timeout after 5 minutes of inactivity

# Set custom timeout and continue on errors
jiji server exec "apt update && apt upgrade -y" --timeout 600 --continue-on-error
```

### Network Management

Manage private networking infrastructure for secure container-to-container communication:

```bash
# View network topology and status
jiji network status

# Tear down the private network infrastructure
jiji network teardown
```

The private network feature provides:

- **WireGuard mesh VPN** for encrypted communication between servers
- **Automatic service discovery via DNS** - containers can connect using service names (e.g., `api.jiji`, `postgres.jiji`)
- **Container-to-container networking** across multiple hosts with automatic DNS resolution
- **Zero-trust security** with encryption by default
- **Daemon-level DNS configuration** for seamless service discovery across all containers

See [Network Reference](docs/network_reference.md) for detailed configuration and usage.

### Deployment Lock Management

Manage deployment locks to prevent concurrent deployments:

```bash
# Acquire a deployment lock
jiji lock acquire "Deploying version 2.0"

# Release the deployment lock
jiji lock release

# Check lock status
jiji lock status

# Show detailed lock information
jiji lock show
```

Deployment locks prevent race conditions when multiple users or CI/CD pipelines attempt to deploy simultaneously.

### Server-Side Audit Trail

View operations history and audit logs from your servers:

```bash
# View recent audit entries from all servers
jiji audit

# View entries from a specific server
jiji audit --host server1.example.com

# Filter by action type across all servers
jiji audit --filter bootstrap

# Filter by status across all servers
jiji audit --status failed

# Aggregate logs chronologically from all servers
jiji audit --aggregate

# View raw log format
jiji audit --raw
```

The audit trail tracks all Jiji operations including:

Server bootstrapping (start, success, failure)
Container engine installations on each server
Service deployments per server
Configuration changes
SSH connections and errors

Audit logs are stored in `.jiji/audit.txt` on each target server and include:

Timestamps (ISO 8601 format)
Action types and status
Server-specific operation context
Detailed error messages and troubleshooting information
Host identification for multi-server deployments

### Global Options

Several global options are available for all commands:

```bash
# Enable verbose logging
jiji --verbose deploy

# Specify a specific app version
jiji --version=v1.2.3 deploy

# Use a custom config file
jiji --config-file=/custom/path/deploy.yml deploy

# Specify environment (uses jiji.<environment>.yml)
jiji --environment=staging deploy

# Target specific hosts (supports wildcards with *)
jiji --hosts="server*.example.com" deploy

# Target specific services (supports wildcards with *)
jiji --services="web*" deploy
```

### Help

Get help for any command:

```bash
jiji --help
jiji server --help
jiji server bootstrap --help
jiji server exec --help
jiji deploy --help
jiji build --help
jiji remove --help
jiji registry --help
```

## Configuration

Jiji uses YAML configuration files (default: `.jiji/deploy.yml`) to define your infrastructure. A typical configuration includes:

Project name
Builder configuration (local or remote builds)
SSH connection settings
Container engine selection (Docker/Podman)
Registry configuration (local or remote with auto-detection)
Private networking settings (WireGuard mesh, service discovery)
Service definitions with images, ports, mounts, environment variables, and proxy settings

For a comprehensive configuration example with all available options and detailed
explanations, see [src/jiji.yml](src/jiji.yml).

Example service with proxy configuration:

```yaml
services:
  web:
    # Build from local Dockerfile
    build:
      context: .
      dockerfile: Dockerfile
    # Or use pre-built image
    # image: nginx:latest
    hosts: [server1.example.com, server2.example.com]
    ports: ["3000:80"]
    volumes:
      - "/data/web/logs:/var/log/nginx"
    environment:
      - ENV=production
    proxy:
      enabled: true
      host: myapp.example.com
      ssl: false
      health_check: "/health"
    files:
      - source: "./config/nginx.conf"
        destination: "/etc/nginx/nginx.conf"
    directories:
      - "/app/uploads"
```

## Development

This project is built with Deno.

### Prerequisites

- [Deno](https://deno.land/) 2.5+

### Running Locally

```bash
# Clone the repository
git clone https://github.com/acidtib/jiji.git
cd jiji

# Run the CLI
deno task run

# Or run directly
deno run src/main.ts
```

## Contributing

1. Fork the repository
2. Create your feature branch (`git checkout -b feature/amazing-feature`)
3. Commit your changes (`git commit -m 'Add some amazing feature'`)
4. Push to the branch (`git push origin feature/amazing-feature`)
5. Open a Pull Request

## License

MIT License - see the [LICENSE](LICENSE) file for details.

## Documentation

For developers and contributors, additional documentation can be found in the
[`docs/`](docs/) directory, which contains detailed information that may be
useful for development and contribution workflows.

## Support

- [Documentation](https://github.com/acidtib/jiji)
- [Issues](https://github.com/acidtib/jiji/issues)
- [Discussions](https://github.com/acidtib/jiji/discussions)
