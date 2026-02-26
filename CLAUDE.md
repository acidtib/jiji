# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with
code in this repository.

## Monorepo Structure

This is a Deno workspace monorepo with three packages:

```
packages/
├── cli/       # @jiji/cli — Main CLI tool (Cliffy, SSH, deployment)
├── dns/       # @jiji/dns — DNS server for service discovery
└── daemon/    # @jiji/daemon — Network reconciliation daemon
```

All packages share a unified version number managed by `mise run version`.

## Workspace Conventions

- `nodeModulesDir` and `unstable` must be in root `deno.json` (Deno workspace
  requirement) — member configs cannot override these
- `@std/assert` is declared in root `deno.json` and inherited by all members —
  don't re-declare it in package configs
- `allowScripts` for npm packages with native builds lives in root config
- Tests run from the package directory (`cd packages/cli && deno task check`),
  so relative paths in tests resolve from there

## Quick Start Commands

```bash
# Install dependencies (run from repo root)
deno install --allow-scripts=npm:cpu-features,npm:ssh2

# Build (via mise — runs from repo root)
mise build              # Build all 3 binaries → target/
mise build:release      # Cross-compile all release binaries

# CLI (via mise)
mise cli:run            # Run CLI
mise cli:build          # Build CLI binary
mise cli:install        # Build + install to ~/.local/bin/jiji
mise cli:build:dev      # Build dev binary
mise cli:install:dev    # Build dev binary + install to ~/.local/bin/jiji_dev
mise cli:release        # Cross-compile CLI for all platforms

# DNS (via mise)
mise dns:run            # Run DNS server
mise dns:build          # Build DNS binary
mise dns:release        # Cross-compile DNS for Linux

# Daemon (via mise)
mise daemon:run         # Run daemon
mise daemon:build       # Build daemon binary
mise daemon:release     # Cross-compile daemon for Linux

# Checks and tests
mise check              # fmt --check + lint + test (CLI package)
mise test               # Run CLI tests only
mise fmt                # Format CLI code
mise lint               # Lint CLI code
deno test --allow-all packages/cli/tests/deploy_plan_test.ts  # Single test file

# Per-package checks (via deno task — each package keeps a `check` task)
cd packages/cli && deno task check
cd packages/dns && deno task check
cd packages/daemon && deno task check

# Version management
mise run version                    # Show current version
mise run version -- --bump          # Auto-increment patch version
mise run version -- --bump 1.0.0    # Set specific version (updates all 3 packages)
```

## High-Level Architecture

### Command Flow (CLI)

The CLI uses Cliffy for command routing. Each command follows this pattern:

```
Command → setupCommandContext() → Load Config → Filter Hosts/Services → SSH Setup → Execute → Cleanup
```

Commands are defined in `packages/cli/src/main.ts` and handlers live in
`packages/cli/src/commands/`. The `setupCommandContext()` helper in
`packages/cli/src/utils/command_helpers.ts` handles common setup.

### Configuration System

Configuration classes in `packages/cli/src/lib/configuration/` use
**lazy-loaded, cached properties**. Validation happens at property access time,
not construction.

All configuration classes extend `BaseConfiguration`. The main classes are:

- `Configuration` - Main entry point, accesses other configs via getters
- `BuilderConfiguration` - Build settings, accesses `RegistryConfiguration`
- `RegistryConfiguration` - Registry authentication and server settings
- `SSHConfiguration` - SSH connection settings
- `NetworkConfiguration` - WireGuard mesh network settings
- `ServersConfiguration` - Server definitions
- `ServiceConfiguration` - Per-service config, accesses `ProxyConfiguration`
- `ProxyConfiguration` - kamal-proxy routing and health checks
- `EnvironmentConfiguration` - Environment variables (shared and per-service)

Config loader searches upward from cwd for `.jiji/deploy.yml` or
`jiji.{environment}.yml`.

### Zero-Downtime Deployment Strategy

Critical flow in `ContainerDeploymentService`:

1. Rename old container (`web` → `web_old_{timestamp}`) - keeps it running
2. Deploy new container with original name
3. Wait for health checks (proxy endpoint or container readiness)
4. Update proxy routing to new container
5. Stop and remove old container
6. Cleanup old images (keep N versions based on `retain` setting)

If health checks fail, old container continues serving traffic.

### Distributed Networking Stack

Three components work together in `packages/cli/src/lib/network/`:

**WireGuard** (`wireguard.ts`): Encrypted mesh VPN

- Per-server /24 subnet from cluster CIDR (e.g., 10.210.0.0/16)
- IPv6 CRDT identifiers for unique peer identification

**Corrosion** (`corrosion.ts`): Distributed CRDT-based SQLite

- No central coordinator - eventual consistency
- Tables: `servers`, `containers`, `services`, `cluster_metadata`
- Gossip on port 31280, API on port 31220

**jiji-dns** (`dns.ts`): DNS-based service discovery

- Resolves `<project>-<service>.jiji` to container IPs
- Queries Corrosion for healthy containers
- Daemon-level DNS configuration (all containers auto-configured)

### SSH Connection Management

`SSHConnectionPool` in `packages/cli/src/utils/ssh_pool.ts` uses a semaphore for
concurrency control (default: 30 concurrent connections).

**Patterns:**

- `executeConcurrent()` - Acquire permit before SSH operation, prevents server
  overload

Related: `executeBestEffort()` in `command_helpers.ts` wraps SSH commands for
cleanup operations — logs failures but doesn't block execution.

### Service Orchestration

Key services in `packages/cli/src/lib/services/`:

- `DeploymentOrchestrator` - High-level deployment coordination
- `ContainerDeploymentService` - Container lifecycle with zero-downtime
- `ProxyService` - kamal-proxy management (SSL termination, routing)
- `BuildService` - Image building and pushing
- `ImagePruneService` - Post-deployment cleanup

### Proxy Configuration

Services deploy to kamal-proxy as `{project}-{service}-{app_port}`.

**Single target:** One port per service **Multi-target:** Multiple ports per
service (e.g., S3 API + Admin API)

Each target's `app_port` must exist in the service's `ports` array.

### Service Runtime Options

Services support these container runtime options (in `ServiceConfiguration`):

- `command` - Override container entrypoint
- `network_mode` - Container network mode (bridge/host/none)
- `cpus`, `memory` - Resource limits
- `gpus`, `devices` - Hardware access
- `privileged`, `cap_add` - Security capabilities
- `stop_first` - Stop old container before starting new (for stateful services)

### Environment Variable Resolution

Secrets use ALL_CAPS pattern and are resolved from:

1. `.env` or `.env.{environment}` files in project root
2. Host environment (with `--host-env` flag)

Registry passwords also support this pattern (e.g., `password: GITHUB_TOKEN`).

### Naming Conventions

- **Images**: `{registry}/{project}-{service}:{version}`
- **Containers**: `{project}-{service}` (old containers get `_old_{timestamp}`
  suffix)
- **Proxy targets**: `{project}-{service}-{app_port}`
- **DNS records**: `{project}-{service}.jiji`

## Key Files

- `packages/cli/src/jiji.yml` - Authoritative configuration reference with all
  options
- `packages/cli/src/constants.ts` - System constants (ports, timeouts, defaults)
- `packages/cli/src/types/` - TypeScript interfaces for all major types
- `packages/cli/tests/mocks.ts` - Mock SSH manager for testing without real
  connections

## Testing

Tests use `MockSSHManager` from `packages/cli/tests/mocks.ts` which simulates
SSH operations via an internal command→response map. Stubbed commands are
matched and responses returned without real SSH connections.

Pattern for adding new tests:

1. Create `new MockSSHManager("hostname")` for each host
2. Use `addMockResponse(commandPattern, response)` to stub SSH commands
3. Cast with `mock as any` when passing to functions expecting `SSHManager`

Run specific test with:

```bash
deno test --allow-all packages/cli/tests/zero_downtime_deployment_test.ts
```

## DNS & Daemon Packages

See `packages/dns/CLAUDE.md` and `packages/daemon/CLAUDE.md` for
package-specific architecture, commands, and gotchas.

## Code Guidelines

- Conform to codebase conventions: follow existing patterns, helpers, naming,
  and formatting; if you must diverge, state why
- Optimize for correctness and clarity; avoid risky shortcuts or speculative
  changes
- Keep type safety: changes should pass `mise build` and type-check; prefer
  proper types over `any` casts
- DRY: search for prior art before adding new helpers or logic; reuse or extract
  shared helpers instead of duplicating
- Tight error handling: no broad catches or silent defaults; propagate or
  surface errors explicitly
- Actionable error messages: every user-facing error must tell the user what to
  DO, not just what went wrong
- Efficient edits: read enough context before changing a file; batch logical
  edits together instead of many tiny patches

## Git Discipline

- Run `/resume-work` at the start of a session to pick up context from previous
  sessions
- Never use `git commit --no-verify` — if hooks fail, fix every issue before
  committing
- Never use destructive commands (`git reset --hard`, `git checkout --`) unless
  explicitly approved
- Never force push to main
- No revert commits for unpushed work: use `git reset HEAD~1` instead of
  `git revert`
- Do not amend a commit unless explicitly requested
- Treat all ESLint warnings as bugs — run `mise lint` and fix before committing
- OSV scanner findings are blockers: run `mise scan` and use `/fix-osv-finding`
  to remediate; never dismiss without analyzing reachability

## Workflow

- Default expectation: deliver working code, not just a plan
- When working within the existing design system, preserve established patterns
  and visual language
- Commit at logical stopping points using `/commit`
- Pause after completing a task and wait for input before continuing
