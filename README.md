<p align="center">
  <img src="docs/jiji_logo.svg" alt="Jiji Logo" width="400">
</p>

# Jiji

Deploy containerized apps across servers with simplicity, speed, and
portability.

## Features

- **Zero downtime deployments** with health checks and automatic rollback
- **Private networking** via WireGuard mesh with automatic DNS service discovery
- **Multi server support** with parallel SSH execution
- **Container engine agnostic** works with Docker or Podman
- **kamal-proxy integration** for HTTP/HTTPS routing and SSL termination

## Installation

### From JSR

```bash
deno install --allow-all --name jiji jsr:@jiji/cli
```

### Linux/macOS

```bash
curl -fsSL https://get.jiji.run/install.sh | sh
```

### Windows

Download from [releases](https://github.com/acidtib/jiji/releases) and add to
PATH.

## Quick Start

```bash
# Create configuration
jiji init

# Edit .jiji/deploy.yml with your servers and services

# Initialize servers (installs container runtime, networking)
jiji server init

# Build and deploy
jiji deploy --build
```

## Commands

| Command                 | Description                               |
| ----------------------- | ----------------------------------------- |
| `jiji init`             | Create config stub in `.jiji/deploy.yml`  |
| `jiji build`            | Build container images                    |
| `jiji deploy`           | Deploy services to servers                |
| `jiji services logs`    | View service logs                         |
| `jiji services restart` | Restart services                          |
| `jiji services remove`  | Remove services                           |
| `jiji services prune`   | Clean up old images                       |
| `jiji proxy logs`       | View kamal-proxy logs                     |
| `jiji server init`      | Initialize servers with container runtime |
| `jiji server exec`      | Execute commands on servers               |
| `jiji server teardown`  | Remove all jiji components from servers   |
| `jiji registry setup`   | Setup container registry                  |
| `jiji network status`   | Show private network status               |
| `jiji network dns`      | Show DNS records                          |
| `jiji network gc`       | Garbage collect stale records             |
| `jiji audit`            | Show deployment audit trail               |
| `jiji lock`             | Manage deployment locks                   |
| `jiji secrets print`    | Print resolved secrets for debugging      |

### Global Options

```bash
-v, --verbose          # Detailed logging
-q, --quiet            # Minimal output
-e, --environment      # Use jiji.<env>.yml config
-H, --hosts            # Target specific hosts (supports wildcards)
-S, --services         # Target specific services (supports wildcards)
--host-env             # Fallback to host env vars when secrets not in .env
```

## Configuration

Configuration lives in `.jiji/deploy.yml`. Example:

```yaml
project: myapp

ssh:
  user: deploy

builder:
  local: true
  engine: docker
  registry:
    type: remote
    server: ghcr.io
    username: yourname
    password: GITHUB_TOKEN
servers:
  server1:
    host: server1.example.com
  server2:
    host: server2.example.com

services:
  web:
    build:
      context: .
      dockerfile: Dockerfile
    hosts:
      - server1
      - server2
    ports:
      - "3000"
    proxy:
      app_port: 3000
      host: myapp.example.com
      ssl: true
      healthcheck:
        path: "/health"
    environment:
      clear:
        NODE_ENV: production
      secrets:
        - DATABASE_URL
```

See [src/jiji.yml](src/jiji.yml) for complete configuration reference.

## Documentation

Detailed guides in [docs/](docs/):

## Development

```bash
# Run CLI
deno task run

# Run tests
deno task test

# Run single test
deno test --allow-all tests/deploy_plan_test.ts

# Format and lint
deno task fmt
deno task lint

# Run all checks
deno task check

# Build binary
deno task build

# Install to /usr/local/bin
deno task install
```

## License

jiji is released under the [MIT License](LICENSE)
