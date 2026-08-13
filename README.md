<p align="center">
  <img src="lib/assets/jiji_logo.svg" alt="Jiji Logo" width="400">
</p>

# Jiji

**Your apps. Your infrastructure. Anywhere.**

Jiji makes it simple to deploy and run containerized applications across the
Linux servers you control.

Cloud VMs, bare metal, dedicated servers, or your own hardware. Docker or
Podman. One workflow for health-gated deployments, automatic HTTPS, private
networking, and service discovery without locking your infrastructure to a
platform.

## Features

- **Fail-safe rolling deployments** that remove failed candidates while the healthy version keeps serving
- **Private networking** via WireGuard mesh with automatic DNS service discovery
- **Multi-server support** with parallel SSH execution
- **SSH config and ProxyJump support** without an SSH subprocess
- **Local registry deployments** through per-host reverse SSH tunnels
- **Docker or Podman** with the same deployment configuration
- **jiji-proxy** for HTTP, HTTPS, and raw TCP routing with mesh-wide load balancing
- **Automatic TLS** for configured domains, plus custom certificate support
- **Scheduled jobs** in isolated one-off service containers
- **Automatic image retention** that continuously prunes old build tags, no separate command needed
- **One-command upgrades** for the local CLI (`jiji update`) and remote agent/proxy components (`jiji server upgrade`)

Rolling services keep the previous version active until its replacement passes
health checks. Services using `stop_first` or direct host-port bindings use a
brief stop-then-start window instead.

## Installation

```bash
curl -fsSL https://get.jiji.run/install.sh | sh
```

Installs the latest release to `~/.local/bin/jiji` (Linux/macOS,
x86_64/arm64). Pin a version with `VERSION=v1.2.3`. To build from source
instead, see Development below. To update an existing install, run
`jiji update`.

## Quick Start

```bash
# Create configuration
jiji init

# Edit .jiji/deploy.yml with your servers and services

# Initialize servers (installs container runtime and complete networking)
jiji server setup

# Build and deploy services with `build:` configuration
jiji deploy --build

# Remove this project's services, network, agent, and unused proxy resources
jiji server teardown

# Remove a local registry container when it is no longer needed
jiji registry teardown
```

### Global Options

```bash
-v, --verbose          # Detailed logging
-q, --quiet            # Minimal output
-c, --config           # Path to config file
-e, --environment      # Use jiji.<env>.yml config
-H, --hosts            # Target specific hosts (supports wildcards)
-S, --services         # Target specific services (supports wildcards)
--host-env             # Fallback to host env vars when secrets not in .env
--version              # Run commands against a specific app version
```

## Configuration

Configuration lives in `.jiji/deploy.yml`. Example:

```yaml
project: myapp

ssh:
  user: deploy

builder:
  engine: docker
  registry:
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
    servers:
      - server1
      - server2
    ports:
      - "3000"
    proxy:
      port: 3000
      hosts: [myapp.example.com]
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

Full guides, configuration reference, and troubleshooting: **[jiji.run/docs](https://jiji.run/docs)**

## Development

This is a Cargo workspace with eight crates in `crates/`: `jiji-core`,
`jiji-tui`, `jiji-config`, `jiji-network`, `jiji-ssh`, `jiji-agent`,
`jiji-proxy`, `jiji-cli` (binary name `jiji`, plus a `jiji_dev` binary for
local iteration).

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
mise build
mise test    # runs via cargo-nextest if installed, else falls back to cargo test
mise fmt
mise lint
mise check
```

## License

Jiji is released under the [MIT License](LICENSE)
