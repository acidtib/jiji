<p align="center">
  <img src="docs/jiji_logo.svg" alt="Jiji Logo" width="400">
</p>

# Jiji

> **WIP**: Under heavy development, not production ready.

Deploy containerized apps across servers with simplicity, speed, and
portability. No infrastructure vendor lock in required.

## Features

**Service Management**: Build, deploy, and remove containerized services across
multiple servers

**Server Initialization**: Initialize servers with curl and Podman or Docker

**Private Networking**: WireGuard mesh VPN with automatic service discovery via
DNS for secure container to container communication across the cluster

**Deployment Locks**: Prevent concurrent deployments with distributed lock
management

**Remote Command Execution**: Execute custom commands across multiple servers

**Configuration Management**: Create and manage infrastructure configurations

**Server Side Audit Trail**: Logging of all operations directly on target
servers

**Registry Management**: Manage container registries (local and remote) with
automatic namespace detection for GHCR and Docker Hub

**Proxy Integration**: Built in support for kamal-proxy for routing traffic to
services

**Mount Management**: Support for file, directory, and volume mounts

**CLI Interface**: Easy to use command line interface built with Cliffy

## Installation

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
curl -fsSL https://get.jiji.run/install.sh | VERSION=v1.2.3 sh
```

### Windows

Download latest version to current directory
[releases page](https://github.com/acidtib/jiji/releases) and add it to your
PATH:

1. Download `jiji-windows-x86_64.exe` from the latest release
2. Rename it to `jiji.exe`
3. Place it in a directory that's in your PATH (e.g., `C:\Windows\System32` or
   create a dedicated folder and add it to PATH)

Or use PowerShell to download and install:

```powershell
# Download to current directory
Invoke-WebRequest -Uri "https://github.com/acidtib/jiji/releases/download/v0.1.13/jiji-windows-x86_64.exe" -OutFile "jiji.exe"
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
# Deploy all services (shows confirmation prompt with deployment plan)
jiji deploy

# Skip confirmation prompt (useful for CI/CD)
jiji deploy --yes

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

The deploy command displays a deployment plan before proceeding, showing which
services will be deployed, build configurations, and target hosts. Use `--yes`
to skip the confirmation prompt.

**Zero Downtime Deployments**: Jiji employs a deployment strategy to ensure no
service interruption:

1. New containers are deployed alongside existing ones
2. Health checks verify the new containers are ready (via proxy health endpoint
   or container readiness)
3. Once healthy, traffic is routed to new containers
4. Old containers are gracefully stopped and removed
5. Old images are cleaned up (keeping configured number of recent versions)

This ensures your service remains available throughout the entire deployment
process.

### Remove Services

Remove services and clean up project directories:

```bash
# Remove all services and project directory (prompts for confirmation)
jiji remove

# Remove without confirmation prompt
jiji remove --confirmed

# Remove specific services only (partial removal - keeps other services running)
jiji remove --services "web,api"

# Partial removal without confirmation
jiji remove --services "web" --confirmed
```

**Note**: When using `--services` to specify particular services, only those
services are removed while other services and the project directory remain
intact. Without `--services`, the entire project is removed from all servers.

### Service Management

Manage running services with restart, cleanup, and logging operations:

```bash
# Restart specific services (stops, removes, and redeploys containers)
jiji services restart --services "web,api"

# Restart services on specific hosts
jiji services restart --hosts "server1.example.com"

# Restart by combining both filters
jiji services restart --services "web" --hosts "server1.example.com"

# Clean up old container images (keeps last 5 versions by default)
jiji services prune

# Keep a specific number of recent image versions
jiji services prune --retain 10

# Skip cleanup of dangling (untagged) images
jiji services prune --no-dangling

# View logs from services
jiji services logs --services "web"

# View last 50 lines from services
jiji services logs --services "api" --lines 50

# Follow logs in real-time from primary server
jiji services logs --services "web" --follow

# Filter logs with grep
jiji services logs --services "api" --grep "ERROR"

# View logs since a specific time (timestamp or relative)
jiji services logs --services "web" --since "2023-01-01T00:00:00Z"
jiji services logs --services "web" --since "30m"

# View logs from a specific container by ID
jiji services logs --container-id abc123def456
```

**Note**: The `restart` command requires either `--hosts` or `--services` to be
specified to prevent accidental restarts of all services. Image pruning
automatically runs after deployments to manage disk space, keeping the
configured number of recent versions while removing older images. The `logs`
command requires `--services` to be specified or `--container-id` for a specific
container.

### Proxy Management

Manage and monitor kamal-proxy for HTTP/HTTPS traffic routing:

```bash
# View logs from kamal-proxy
jiji proxy logs

# View last 50 lines from kamal-proxy
jiji proxy logs --lines 50

# Follow kamal-proxy logs in real-time
jiji proxy logs --follow

# Filter proxy logs with grep
jiji proxy logs --grep "ERROR"

# View proxy logs since a specific time
jiji proxy logs --since "1h"
jiji proxy logs --since "2023-01-01T00:00:00Z"

# View proxy logs from specific hosts
jiji proxy logs --hosts "server1.example.com"
```

The proxy logs command helps monitor HTTP/HTTPS traffic routing, debug routing
issues, and track requests flowing through kamal-proxy.

### Registry Management

Manage container registries for storing and retrieving images with automatic
configuration:

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

**Auto Detection Support**: Jiji automatically detects namespace requirements
for supported registries:

- **GHCR** (`ghcr.io`): Auto namespace as `username/project-name`
- **Docker Hub** (`docker.io`): Auto namespace as `username`
- **Local registries**: No namespace required

See [Registry Reference](docs/registry-reference.md) for detailed configuration
examples.

### Server Management

Initialize servers with container runtime:

```bash
jiji server init
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

Tear down server infrastructure and remove all Jiji related components:

```bash
# Remove all Jiji components from servers (prompts for confirmation)
jiji server teardown

# Teardown without confirmation prompt
jiji server teardown --confirmed

# Teardown specific hosts only
jiji server teardown --hosts "server1.example.com"
```

**Warning**: The `server teardown` command removes all containers, networks,
volumes, and configuration files created by Jiji. This operation cannot be
undone.

### Network Management

Manage private networking infrastructure for secure container to container
communication:

```bash
# View network topology and status
jiji network status

# Tear down the private network infrastructure
jiji network teardown
```

The private network feature provides:

- **WireGuard mesh VPN** for encrypted communication between servers
- **Automatic service discovery via DNS** - containers can connect using service
  names (e.g., `api.jiji`, `postgres.jiji`)
- **Container to container networking** across multiple hosts with automatic DNS
  resolution
- **Zero trust security** with encryption by default
- **Daemon level DNS configuration** for seamless service discovery across all
  containers

See [Network Reference](docs/network-reference.md) for detailed configuration
and usage.

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

Deployment locks prevent race conditions when multiple users or CI/CD pipelines
attempt to deploy simultaneously.

### Server Side Audit Trail

View operations history and audit logs from your servers:

```bash
# View recent audit entries from all servers
jiji audit

# View entries from a specific server
jiji audit --host server1.example.com

# Filter by action type across all servers
jiji audit --filter init

# Filter by status across all servers
jiji audit --status failed

# Aggregate logs chronologically from all servers
jiji audit --aggregate

# View raw log format
jiji audit --raw
```

The audit trail tracks all Jiji operations including:

- Server initialization (start, success, failure)
- Container engine installations on each server
- Service deployments per server
- Configuration changes
- SSH connections and errors

Audit logs are stored in `.jiji/audit.txt` on each target server and include:

- Timestamps (ISO 8601 format)
- Action types and status
- Server specific operation context
- Detailed error messages and troubleshooting information
- Host identification for multi server deployments

### Global Options

Several global options are available for all commands:

```bash
# Enable verbose logging
jiji --verbose deploy

# Enable quiet mode (minimal output)
jiji --quiet deploy

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
jiji services --help
jiji services restart --help
jiji services prune --help
jiji services logs --help
```

## Deployment Workflow

This section provides an end to end example of deploying an application with
Jiji, from initial setup to monitoring a production deployment.

### Initial Setup

1. **Initialize your configuration**

```bash
# Create .jiji/deploy.yml configuration file
jiji init
```

2. **Edit your configuration** in `.jiji/deploy.yml`:

```yaml
project: myapp

ssh:
  user: deploy

builder:
  local: true # Build images on your local machine
  engine: docker

registry:
  server: ghcr.io
  username: yourname
  password: password

services:
  web:
    build:
      context: .
      dockerfile: Dockerfile
    servers:
      - host: server1.example.com
    ports:
      - "3000:3000"
    proxy:
      enabled: true
      hosts:
        myapp.example.com
      ssl: true
      health_check:
        path: "/health"
```

3. **Initialize your servers**

```bash
# Install container runtime and setup infrastructure
jiji server init
```

### First Deployment

1. **Build your images**

```bash
# Build and push images to registry
jiji build
```

2. **Deploy your services**

```bash
# Deploy with confirmation prompt showing deployment plan
jiji deploy

# Review the deployment plan, then confirm
# Jiji will:
# - Install kamal-proxy on servers (if services use proxy)
# - Deploy containers with zero downtime rollout
# - Configure proxy routing with health checks
# - Clean up old containers after successful deployment
```

3. **Monitor the deployment**

```bash
# Follow logs in real time
jiji services logs --services "web" --follow

# Check deployment status
jiji server exec "docker ps"

# Verify health check
curl https://myapp.example.com/health
```

### Updating Your Application

When you want to deploy an update:

```bash
# Build and deploy in one command
jiji deploy --build

# Or with a custom version tag
jiji deploy --build --version v2.0.0

# Skip confirmation for CI/CD
jiji deploy --build --yes
```

**Zero Downtime Deployments**: Jiji keeps old containers running until new ones
pass health checks, ensuring no service interruption during updates.

### Common Workflows

**Deploy to specific environment**:

```bash
# Use staging configuration
jiji --environment staging deploy

# Use production configuration
jiji --environment production deploy
```

**Deploy specific services**:

```bash
# Only deploy the API service
jiji deploy --services "api"

# Deploy multiple services
jiji deploy --services "web,api"
```

**View logs from deployment**:

```bash
# Last 100 lines from web service
jiji services logs --services "web" --lines 100

# Filter for errors
jiji services logs --services "web" --grep "ERROR"

# Since deployment started (30 minutes ago)
jiji services logs --services "web" --since "30m"
```

**Restart a service**:

```bash
# Restart web service on all hosts
jiji services restart --services "web"

# Restart on specific host only
jiji services restart --services "web" --hosts "server1.example.com"
```

**Clean up old images**:

```bash
# Remove old image versions (keeps last 5)
jiji services prune

# Keep more versions
jiji services prune --retain 10
```

### Advanced Workflows

For more advanced deployment patterns including multi environment setups, CI/CD
integration, and rollback procedures, see
[docs/deployment-guide.md](docs/deployment-guide.md).

## Configuration

Jiji uses YAML configuration files (default: `.jiji/deploy.yml`) to define your
infrastructure. You can also use environment-specific configs like
`jiji.staging.yml` or `jiji.production.yml` with the `--environment` flag.

### SSH Configuration

Jiji supports flexible SSH configuration for connecting to remote servers:

```yaml
ssh:
  user: deploy
  # Use SSH agent authentication (default)
  use_ssh_agent: true

  # Or specify private keys
  private_keys:
    - ~/.ssh/id_rsa
    - ~/.ssh/id_ed25519

  # Use ProxyJump for bastion hosts
  proxy_jump: bastion.example.com

  # Or ProxyCommand for advanced scenarios
  proxy_command: "ssh -W %h:%p bastion.example.com"

  # Connection timeouts
  timeout: 30000 # milliseconds
```

Jiji also parses your `~/.ssh/config` file automatically, respecting Host
entries, ProxyJump, and IdentityFile settings.

### Builder Configuration

Configure how images are built locally on your machine or remotely on target
servers:

```yaml
builder:
  # Local builds (default) - builds on your local machine
  local: true

  # Or remote builds - builds on each target server
  # local: false

  # Container engine selection
  engine: docker # or "podman"
```

### Environment Variables

Jiji automatically converts non string types (numbers, booleans) to strings for
container compatibility:

```yaml
# Shared environment variables applied to all services
environment:
  # Clear text environment variables
  clear:
    API_URL: https://api.example.com
    DEBUG: true
    PORT: 3000
  # Secrets loaded from host environment variables
  secrets:
    - API_KEY
    - DATABASE_PASSWORD
```

Service level environment variables can also use this auto conversion feature.

### Example Service Configuration

Example service with configuration options:

```yaml
services:
  web:
    # Build from local Dockerfile
    build:
      context: .
      dockerfile: Dockerfile
      args:
        - NODE_ENV=production
    # Or use pre-built image
    # image: nginx:latest

    servers:
      - host: server1.example.com
      - host: server2.example.com
    ports:
      - "3000:80"

    volumes:
      - "/data/web/logs:/var/log/nginx"
      - "web-cache:/tmp/cache"

    environment:
      clear:
        ENV: production
      secrets:
        - API_KEY # Environment variable substitution from host

    proxy:
      enabled: true
      hosts:
        - myapp.example.com
        - www.myapp.example.com # Multiple hosts
      ssl: true
      health_check:
        path: "/health"
        interval: "10s"
        timeout: "5s"
        deploy_timeout: "60s"

    # Files: Upload from local repo to host .jiji/{project}/files/ before mounting
    # String format: local:remote[:options] where options can be ro, z, or Z
    files:
      - "nginx.conf:/etc/nginx/nginx.conf:ro"

    # Directories: Created on host .jiji/{project}/directories/ before mounting
    # String format: local:remote[:options] where options can be ro, z, or Z
    directories:
      - "html:/usr/share/nginx/html:ro"
```

### Complete Configuration Reference

For a configuration example with all available options and detailed
explanations, see:

- [src/jiji.yml](src/jiji.yml) - Authoritative reference configuration
- [docs/configuration-reference.md](docs/configuration-reference.md) -
  Configuration guide

## Troubleshooting

### Common Issues

#### SSH Connection Failures

```bash
# Use verbose mode to see detailed SSH connection information
jiji --verbose server exec "whoami"

# Verify SSH config is correct
ssh -v user@server1.example.com

# Check if ProxyJump is configured correctly in ~/.ssh/config
```

If you encounter "Permission denied" errors, ensure:

- Your SSH key is added to the server's `~/.ssh/authorized_keys`
- The SSH agent is running: `eval $(ssh-agent)` and `ssh-add ~/.ssh/id_rsa`
- Your user has sudo permissions on target servers

#### Registry Authentication Issues

```bash
# Verify registry credentials
jiji registry login

# Check if registry URL is correct in deploy.yml
# For GHCR, use: ghcr.io/username/project-name
# For Docker Hub, use: docker.io or leave empty
```

Common registry errors:

- **403 Forbidden**: Check username and ensure it matches the registry namespace
- **401 Unauthorized**: Re run `jiji registry login` with correct credentials
- **Push failed**: Verify you have push permissions to the repository

#### Container Won't Start

```bash
# Check container logs for errors
jiji services logs --services "web" --lines 100

# Verify the container is running
jiji server exec "docker ps -a"

# Check health endpoint if proxy is enabled
curl -I http://server1.example.com/health
```

Common causes:

- Environment variables missing or incorrect
- Volume mount paths don't exist on the server
- Port conflicts with existing containers
- Health check endpoint returning errors

#### Network Connectivity Issues

```bash
# Check network status
jiji network status

# Verify WireGuard is running
jiji server exec "systemctl status wg-quick@jiji"

# Test DNS resolution
jiji server exec "ping api.jiji"

# Check routing table
jiji server exec "ip route show"
```

### Debug Mode

Enable verbose logging for detailed information about command execution:

```bash
jiji --verbose deploy
jiji --verbose network status
jiji --verbose services logs --services "web"
```

### Getting Help

For more detailed troubleshooting guides and solutions, see:

- [docs/troubleshooting.md](docs/troubleshooting.md) - Troubleshooting guide
- [GitHub Issues](https://github.com/acidtib/jiji/issues) - Report bugs or ask
  questions
- [GitHub Discussions](https://github.com/acidtib/jiji/discussions) - Community
  support

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

### Development Commands

```bash
# Run all tests
deno task test

# Format code
deno task fmt

# Lint code
deno task lint

# Run all checks (format check, lint, and tests)
deno task check

# Build compiled binary
deno task build

# Build and install to /usr/local/bin/jiji
deno task install

# Development build/install (outputs jiji_dev)
deno task dev:build
deno task dev:install

# Run specific test file
deno test --allow-all tests/deploy_plan_test.ts
```

### Version Management

Update version across the codebase:

```bash
# Show current version
./bin/version

# Update to specific version
./bin/version 1.2.3

# Auto increment version (patch)
./bin/version
```

This updates `src/version.ts` and `deno.json` automatically. See
[docs/version.md](docs/version.md) for more details.

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
