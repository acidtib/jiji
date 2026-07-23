<p align="center">
  <img src="docs/jiji_logo.svg" alt="Jiji Logo" width="400">
</p>

# Jiji

Deploy containerized apps across servers with simplicity, speed, and
portability.

> **Status:** Jiji is being rewritten from Deno/TypeScript to Rust,
> piece by piece. The Rust CLI currently implements `init`, `server setup`,
> `network plan`, and transactional `network setup`; the remaining command
> surface described below is the target. See
> [docs/superpowers/specs/2026-07-22-rust-rewrite-init-design.md](docs/superpowers/specs/2026-07-22-rust-rewrite-init-design.md)
> for the rewrite plan.

## Features

- **Zero downtime deployments** with health checks and automatic rollback
- **Private networking** via WireGuard mesh with automatic DNS service discovery
- **Multi server support** with parallel SSH execution
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

# Build and deploy
jiji deploy --build
```

## Commands

| Command                 | Description                               | Status      |
| ------------------------ | ----------------------------------------- | ----------- |
| `jiji init`              | Create config stub in `.jiji/deploy.yml`  | Rust        |
| `jiji build`              | Build container images                    | not yet ported |
| `jiji deploy`             | Deploy services to servers                | not yet ported |
| `jiji services logs`     | View service logs                         | not yet ported |
| `jiji services restart`  | Restart services                          | not yet ported |
| `jiji services remove`   | Remove services                           | not yet ported |
| `jiji services prune`    | Clean up old images                       | not yet ported |
| `jiji proxy logs`        | View kamal-proxy logs                     | not yet ported |
| `jiji server setup`      | Install container runtime and private network | Rust |
| `jiji server exec`       | Execute commands on servers               | not yet ported |
| `jiji server teardown`   | Remove all jiji components from servers   | not yet ported |
| `jiji registry setup`    | Setup container registry                  | not yet ported |
| `jiji network plan`      | Print the deterministic private network plan | Rust |
| `jiji network setup`     | Install, update, or repair the private network | Rust |
| `jiji audit`              | Show deployment audit trail                | not yet ported |
| `jiji lock`               | Manage deployment locks                    | not yet ported |
| `jiji secrets print`     | Print resolved secrets for debugging       | not yet ported |

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

See [crates/jiji-config/src/jiji.yml](crates/jiji-config/src/jiji.yml) for the
complete configuration reference (also the template `jiji init` writes).

## Documentation

Detailed guides in [docs/](docs/):

## Development

This is a Cargo workspace with four crates in `crates/`: `jiji-core`,
`jiji-tui`, `jiji-config`, `jiji-cli` (binary name `jiji`).

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
