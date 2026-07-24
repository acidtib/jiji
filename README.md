<p align="center">
  <img src="docs/jiji_logo.svg" alt="Jiji Logo" width="400">
</p>

# Jiji

Deploy containerized apps across servers with simplicity, speed, and
portability.

> **Status:** Jiji has been rewritten from Deno/TypeScript to Rust. The Rust
> CLI implements `init`, `server setup`, `network plan`/`network setup`,
> `build`, `deploy`, `server teardown`, `registry login`/`logout`, and
> `registry teardown` (see the command table below for the full current
> picture). A few operational commands (`services`, `proxy logs`, `audit`,
> `lock`, `secrets print`, `server exec`) are not started yet.

## Features

- **Zero downtime deployments** with health checks and automatic rollback
- **Private networking** via WireGuard mesh with automatic DNS service discovery
- **Multi server support** with parallel SSH execution
- **SSH config and ProxyJump support** without invoking an SSH subprocess
- **Local registry deployments** through per-host reverse SSH tunnels
- **Container engine agnostic** works with Docker or Podman
- **kamal-proxy integration** for HTTP/HTTPS routing and SSL termination

## Installation

Build from source (see Development below). Prebuilt binaries and the install
script are not yet updated for the Rust rewrite.

## Quick Start

```bash
# Create configuration
jiji init

# Edit .jiji/deploy.yml with your servers and services

# Initialize servers (installs container runtime and complete networking)
jiji server setup

# Build and deploy services with `build:` configuration
jiji deploy --build

# Tear down everything jiji installed on selected servers
jiji server teardown

# Remove a local registry container when it is no longer needed
jiji registry teardown
```

## Commands

| Command                 | Description                               | Status      |
| ------------------------ | ----------------------------------------- | ----------- |
| `jiji init`              | Create config stub in `.jiji/deploy.yml`  | Rust        |
| `jiji build`             | Build container images                    | Rust        |
| `jiji deploy`            | Deploy services to servers                | Rust        |
| `jiji services logs`     | View service logs                         | not yet implemented |
| `jiji services restart`  | Restart services                          | not yet implemented |
| `jiji services remove`   | Remove services                           | not yet implemented |
| `jiji services prune`    | Clean up old images                       | not yet implemented |
| `jiji proxy logs`        | View kamal-proxy logs                     | not yet implemented |
| `jiji server setup`      | Install container runtime and private network | Rust |
| `jiji server exec`       | Execute commands on servers               | not yet implemented |
| `jiji server teardown`   | Remove all jiji components from servers   | Rust |
| `jiji registry login`    | Authenticate the local machine and/or servers to the configured registry | Rust |
| `jiji registry logout`   | Remove registry credentials from the local machine and/or servers | Rust |
| `jiji registry setup`    | Setup container registry                  | not yet implemented |
| `jiji registry teardown` | Safely remove the local registry container | Rust |
| `jiji network plan`      | Print the deterministic private network plan | Rust |
| `jiji network setup`     | Install, update, or repair the private network | Rust |
| `jiji audit`              | Show deployment audit trail                | not yet implemented |
| `jiji lock`               | Manage deployment locks                    | not yet implemented |
| `jiji secrets print`     | Print resolved secrets for debugging       | not yet implemented |

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
    # Use a published image, or replace this with `build:` and run
    # `jiji deploy --build`.
    image: ghcr.io/yourname/myapp-web:latest
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

See [crates/jiji-config/src/jiji.yml](crates/jiji-config/src/jiji.yml) for the
complete configuration reference (also the template `jiji init` writes).

## Documentation

Detailed guides in [docs/](docs/), see [docs/README.md](docs/README.md) for
the full index: architecture, configuration reference, network reference
(WireGuard/DNS), deployment guide, and troubleshooting.

## Development

This is a Cargo workspace with six crates in `crates/`: `jiji-core`,
`jiji-tui`, `jiji-config`, `jiji-network`, `jiji-ssh`, `jiji-cli` (binary name
`jiji`, plus a `jiji_dev` binary for local iteration).

```bash
# Run the CLI
cargo run -- init

# Run tests
cargo test

# Format and lint
cargo fmt
cargo clippy --all-targets --all-features

# Build a debug binary
cargo build

# Or via mise (wraps the same cargo commands)
mise run build
mise run test
mise run fmt
mise run lint
mise run check
```

## License

jiji is released under the [MIT License](LICENSE)
