# Jiji Architecture

High level overview of Jiji's system architecture, components, and design
patterns.

## Table of Contents

- [System Overview](#system-overview)
- [Component Architecture](#component-architecture)
- [Deployment Architecture](#deployment-architecture)
- [Network Architecture](#network-architecture)
- [Configuration System](#configuration-system)
- [SSH Management](#ssh-management)
- [Command Layer](#command-layer)
- [Data Flow](#data-flow)
- [Security Model](#security-model)

## System Overview

Jiji is a deployment orchestration tool that manages containerized applications
across multiple servers using a command line interface.

### Core Capabilities

- **Service Deployment**: Zero downtime container deployments with health checks
- **Private Networking**: Per-project WireGuard mesh VPN with automatic service discovery
- **Registry Management**: Local and remote container registry support
- **SSH Orchestration**: Parallel command execution across multiple servers
- **Audit Trail**: Append-only JSONL log of every state-changing operation

### Technology Stack

- **Language/Runtime**: Rust, compiled to a single static binary (`jiji`)
- **CLI Framework**: clap (derive)
- **SSH**: russh (pure-Rust async SSH client, no subprocess, no libssh FFI)
- **Container Runtime**: Docker or Podman
- **Networking**: WireGuard, routed container bridges, dnsmasq, nftables

### Architecture Principles

1. **Zero downtime**: Keep old containers running until new ones are healthy
2. **Idempotent operations**: Commands can be run multiple times safely
3. **Fail safe**: Operations that fail don't leave system in broken state
4. **Distributed first**: Designed for multi server deployments
5. **Configuration as code**: All infrastructure defined in YAML

## Component Architecture

Jiji is a Cargo workspace of six crates. `jiji-cli` is the only crate that
knows about commands, SSH, or containers directly; the crates below it are
each a narrow, independently testable layer.

```
┌──────────────────────────────────────────────────────────────────┐
│                         jiji-cli (binary)                        │
│  cli.rs (clap Commands) -> commands/*::run() -> orchestration    │
│                                                                    │
│  deploy_transaction.rs   service_network.rs   container_runtime.rs│
│  proxy.rs / proxy_routes.rs   audit.rs   lock.rs   mounts.rs      │
│  env_resolution.rs   ssh_adapter.rs   registry.rs                 │
└───────────┬───────────────┬───────────────┬───────────────┬──────┘
            │               │               │               │
            V               V               V               V
     ┌────────────┐  ┌─────────────┐  ┌───────────┐  ┌────────────┐
     │ jiji-config │  │ jiji-network│  │  jiji-ssh │  │  jiji-tui  │
     │ Config      │  │ NetworkPlan │  │ SshSession│  │ Ui::say/   │
     │ schema,     │  │ naming.rs,  │  │ SshPool   │  │ section/   │
     │ load/       │  │ service_    │  │ (russh)   │  │ confirm/   │
     │ validate    │  │ runtime.rs  │  │           │  │ spinner    │
     └─────────────┘  └─────────────┘  └───────────┘  └────────────┘
            │
            V
     ┌─────────────┐
     │  jiji-core  │  pattern matching, error types, default CIDRs
     └─────────────┘
```

Each command's `run()` in `crates/jiji-cli/src/commands/` repeats the same
sequence inline (there is no shared `setupCommandContext()`-style helper):
`load_config()` -> `validate_config()` -> build a `NetworkPlan` (if the
command needs one) -> select hosts (`NetworkPlan::select_hosts`) -> connect
via `SshPool`/`SshSession` -> execute -> close sessions.

```
┌───────────────────────────────────────────────────────────┐
│                    Remote Servers                         │
├───────────────────────────────────────────────────────────┤
│                                                           │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────┐ │
│  │  Docker/     │  │  WireGuard   │  │  kamal-proxy     │ │
│  │  Podman      │  │  Mesh VPN    │  │  HTTP/HTTPS      │ │
│  │  Containers  │  │  (per-project│  │  Routing         │ │
│  │              │  │  interface)  │  │  (shared, multi- │ │
│  │              │  │              │  │  homed per host) │ │
│  └──────────────┘  └──────────────┘  └──────────────────┘ │
│                                                           │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────┐ │
│  │  dnsmasq     │  │  nftables    │  │  Service         │ │
│  │  Compiled    │  │  Stable VIP  │  │  Monitoring      │ │
│  │  .jiji DNS   │  │  Cutover     │  │  & Logs          │ │
│  │  (per project│  │  (per project│  │                  │ │
│  │  resolver)   │  │  table)      │  │                  │ │
│  └──────────────┘  └──────────────┘  └──────────────────┘ │
└───────────────────────────────────────────────────────────┘
```

Every jiji-managed resource on a host except kamal-proxy is scoped to one
`project:` (see [Network Architecture](#network-architecture)) -- two
independent projects can run `jiji server setup` against the same physical
host and get two fully isolated sets of the above.

## Deployment Architecture

### Zero Downtime Deployment Flow

Jiji assigns each service endpoint two fixed backend addresses
(`BackendSlot::A`/`B`) and one stable VIP address up front, computed
deterministically from config alone (`NetworkPlanner`). There is no
rename-based deploy step; the candidate container always starts on whichever
slot isn't currently active.

```
1. Pre Deployment
   ┌────────────────────────────────────────────┐
   │ - Validate configuration                   │
   │ - Reconcile network generation if stale     │
   │ - Acquire deployment lock                   │
   │ - Verify registry authentication            │
   └────────────────────────────────────────────┘
                      │
                      V
2. Candidate Deployment
   ┌────────────────────────────────────────────┐
   │ ACTIVE SLOT (A)      CANDIDATE SLOT (B)     │
   │ Still serving        Being deployed         │
   │ via VIP              at its own fixed IP    │
   │                                             │
   │ ┌──────────┐         ┌──────────┐           │
   │ │ web-a    │         │ web-b    │           │
   │ │ Healthy  │         │ Starting │           │
   │ └──────────┘         └──────────┘           │
   │      │                     │                │
   │      │                     V                │
   │      │       Health check runs directly     │
   │      │       against candidate (not VIP)    │
   │      │                     │                │
   │      │               ┌─────V─────┐          │
   │      │               │ Healthy?  │          │
   │      │               └─────┬─────┘          │
   │      │                     │ Yes             │
   │      │                     V                 │
   │      │        VIP (nftables DNAT) flips      │
   │      │        to candidate's backend         │
   │      │                     │                 │
   │      │        kamal-proxy routes verified    │
   │      │                     │                 │
   │      V                     V                 │
   │ Stop & Remove       Now serving traffic       │
   └────────────────────────────────────────────┘
                      │
                      V
3. Post Deployment
   ┌────────────────────────────────────────────┐
   │ - Release deployment lock                  │
   │ - Log audit trail                          │
   └────────────────────────────────────────────┘
```

If the health check fails, the previous container is never touched and keeps
serving traffic through the still-unflipped VIP; the candidate and any
partial proxy routes are rolled back.

### Health Check System

```
┌──────────────┐
│ jiji CLI     │
└──────┬───────┘
       │
       │ Health check request sent directly
       │ to the candidate's fixed backend IP
       │ (never through the VIP or kamal-proxy)
       │
       V
┌──────────────┐
│  Container   │
│  web-b       │
│              │
│  /health →   │
│  200 OK      │
└──────────────┘
       │
       │ Status: healthy
       │
       V
┌──────────────┐
│ VIP cutover  │
│ + proxy      │
│ route enable │
└──────────────┘
```

### Image Management

```
┌─────────────────────────────────────────────────────┐
│                   Registry                          │
│                                                     │
│  myproject/service:abc1234 <── Current deployment   │
│  myproject/service:def5678 <── Previous version     │
│  myproject/service:ghi9012 <── Old version          │
│  ...                                                │
│  (Older versions cleaned by `jiji service prune`)   │
└─────────────────────────────────────────────────────┘
                      │
                      │ Pull image
                      V
┌─────────────────────────────────────────────────────┐
│                 Local Server                        │
│                                                     │
│  myproject/service:abc1234 <── Running container    │
│  myproject/service:def5678 <── Cached image         │
│  (Older images removed by `jiji service prune`)     │
└─────────────────────────────────────────────────────┘
```

## Network Architecture

The full design (per-project isolation, exact naming derivations, and the
residual hash-collision risk when multiple projects share default CIDR
ranges) is documented in [Network Reference](network-reference.md); this
section is a summary.

### WireGuard Mesh Topology

Every name below (`jiji{8 hex}` interface, `jiji-{slug}` bridge,
`51820..=55819` port range) is derived purely from `config.project`
(`jiji-network/src/naming.rs`), so two projects sharing a host get two fully
independent meshes.

```
┌────────────────────────────────────────────────────────┐
│              WireGuard Mesh VPN (one project)           │
│                                                        │
│  Server 0 (10.210.0.1)                                 │
│      │                                                 │
│      ├─────────┐                                       │
│      │         │                                       │
│      │         │                                       │
│  Server 1  Server 2                                    │
│ (10.210.1.1) (10.210.2.1)                              │
│      │         │                                       │
│      └────┬────┘                                       │
│           │                                            │
│       Server 3                                         │
│     (10.210.3.1)                                       │
│                                                        │
│  Each server:                                          │
│  - Gets its own management subnet slot                 │
│  - Establishes peer connections to all other servers   │
│  - Routes traffic through its per-project WireGuard     │
│    interface (jiji{8 hex}, one per project)             │
└────────────────────────────────────────────────────────┘
```

### Container Networking

```
Server 1 (192.168.1.100)
┌─────────────────────────────────────────────┐
│                                             │
│  ┌────────────┐  ┌────────────┐             │
│  │ Web        │  │ API        │             │
│  │ 10.210.0.2 │  │ 10.210.0.3 │             │
│  └────────────┘  └────────────┘             │
│         │              │                    │
│         └──────┬───────┘                    │
│                │                            │
│         ┌──────V──────┐                     │
│         │ jiji-{slug} │  docker/podman       │
│         │  bridge     │  bridge network      │
│         └──────┬──────┘                     │
│                │                            │
│         ┌──────V──────┐                     │
│         │ jiji{8 hex} │                     │
│         │  WireGuard  │  <───────────┐      │
│         │  10.210.0.1 │              │      │
│         └─────────────┘              │      │
│                                      │      │
└──────────────────────────────────────┼──────┘
                                       │
                    WireGuard Tunnel   │
                                       │
Server 2 (192.168.1.101)               │
┌──────────────────────────────────────┼──────┐
│                                      │      │
│  ┌────────────┐  ┌────────────┐      │      │
│  │ Database   │  │ Cache      │      │      │
│  │ 10.210.1.2 │  │ 10.210.1.3 │      │      │
│  └────────────┘  └────────────┘      │      │
│         │              │             │      │
│         └──────┬───────┘             │      │
│                │                     │      │
│         ┌──────V──────┐              │      │
│         │ jiji-{slug} │              │      │
│         │  bridge     │              │      │
│         └──────┬──────┘              │      │
│                │                     │      │
│         ┌──────V──────┐              │      │
│         │ jiji{8 hex} │  <───────────┘      │
│         │  WireGuard  │                     │
│         │  10.210.1.1 │                     │
│         └─────────────┘                     │
│                                             │
└─────────────────────────────────────────────┘
```

kamal-proxy is the one component that is deliberately shared and
multi-tenant: one container per host, multi-homed across every project's
bridge that has active routes on that host.

### Service Discovery Flow

```
Container Query: "myapp-api.jiji"
         │
         V
┌──────────────────┐
│ Container DNS    │ This project's jiji bridge resolver
│ /etc/resolv.conf │ search domain: jiji
└────────┬─────────┘
         │
         V
┌────────────────┐
│    dnsmasq     │  jiji-dns-{slug}.service, compiled from
│  static zone   │  deploy.yml; returns stable service VIPs
└────────┬───────┘
         │
         │ VIP packet
         V
┌────────────────┐
│   nftables     │  jiji-service-nat-{slug}.service, host-local
│ service NAT    │  atomic map: VIP -> active backend A/B
└────────┬───────┘
         │
         │ Routed through WireGuard when remote
         V
┌────────────────┐
│ Active service │  Fixed backend address
│   container    │  Docker or Podman
└────────────────┘
```

DNS represents configured topology. Deployment health checks gate the
host-local VIP switch, so unhealthy candidates never become active without
requiring live health data in DNS.

## Configuration System

### Schema

Configuration is a plain `serde`-deserializable struct tree
(`jiji-config/src/schema.rs`): `Config`, `NamedServer`, `Ssh`, `Service`,
`ProxyConfig`, `MountConfig`, and friends. There is no lazy-loaded getter
layer or base-class hierarchy -- every field is present (or `Option`) on the
struct as soon as the YAML is parsed, and validation is a separate explicit
pass rather than happening implicitly at property-access time.

```
Config
  ├── project: String
  ├── ssh: Option<Ssh>
  ├── builder: Builder
  │     └── registry: Registry
  ├── network: Option<NetworkConfig>
  ├── servers: BTreeMap<String, NamedServer>
  ├── environment: Option<Environment>   (project-level, shared)
  ├── secrets: Option<SecretsAdapter>    (schema only, see docs/followup.md)
  └── services: BTreeMap<String, Service>
        ├── proxy: Option<ProxyConfig>
        │     └── healthcheck: Option<Healthcheck>   (path or cmd)
        ├── environment: Option<Environment>          (service-specific)
        └── build: Option<BuildConfig>                (context, dockerfile, args, target)
```

Health checks are fields on `ProxyConfig`/`Healthcheck`, not a separate
class. Health checks support two modes:

- HTTP mode: `path` field, checked with an HTTP GET
- Command mode: `cmd` field, a command run inside the container (exit 0 =
  healthy)

### Configuration Loading Flow

```
1. Load YAML File
   ┌─────────────────────┐
   │ .jiji/deploy.yml    │
   │ or                  │
   │ jiji.<env>.yml      │
   └──────────┬──────────┘
              │
              V
2. Parse & Validate
   ┌─────────────────────┐
   │ YAML → Rust structs │
   │ (serde)             │
   │ validate_config()   │
   │ -> ValidationResult │
   └──────────┬──────────┘
              │
              V
3. Secrets Resolution
   ┌─────────────────────┐
   │ Load .env files     │
   │ VAR_NAME → value    │
   │ Optional host-env   │
   │ fallback (--host-env)│
   └──────────┬──────────┘
              │
              V
4. Build Network Plan
   ┌─────────────────────┐
   │ NetworkPlanner::plan │
   │ - WireGuard peers    │
   │ - backend slots/VIPs │
   │ - DNS records        │
   └─────────────────────┘
```

`load_config()` (`jiji-config`) searches upward from the current directory
for `.jiji/deploy.yml` or `jiji.{environment}.yml`. `validate_config()`
returns a `ValidationResult` with explicit errors rather than throwing on the
first problem found.

## SSH Management

### Connection Model

`jiji-ssh` has no persistent connection cache or LRU eviction: each command
invocation opens exactly one `SshSession` per selected server (via
`ssh_adapter::connect_options` + `SshSession::connect`), keeps it open for
the duration of that command, and closes it before the process exits. What
`SshPool` provides is a **concurrency limiter**, not a cache: a
semaphore-backed helper (`execute_concurrent`/`execute_batched`/
`execute_with_error_collection`) that runs independent SSH operations across
many hosts without opening more concurrent connections than
`ssh.max_concurrent_starts` allows.

```
┌────────────────────────────────────────────┐
│         One jiji command invocation        │
├────────────────────────────────────────────┤
│                                            │
│  ┌──────────────────────────────────────┐  │
│  │  SshPool (Semaphore, max N in flight) │  │
│  ├──────────────────────────────────────┤  │
│  │ server1.example.com → SshSession     │  │
│  │ server2.example.com → SshSession     │  │
│  │ server3.example.com → SshSession     │  │
│  │ ...                                  │  │
│  └──────────────────────────────────────┘  │
│                                            │
│  Features:                                 │
│  - russh (pure Rust, no subprocess/FFI)    │
│  - Bounded parallel execution               │
│  - ProxyJump / ProxyCommand support         │
│  - Key file, inline key data, ssh-agent     │
│  - connect_timeout / command_timeout        │
│  - Sessions closed explicitly at the end    │
│    of the command, never reused across      │
│    separate jiji invocations                │
└────────────────────────────────────────────┘
```

### Command Execution Flow

```
1. Command Request
   ┌─────────────────────┐
   │ jiji server exec    │
   │ "docker ps"         │
   └──────────┬──────────┘
              │
              V
2. SSH Connection
   ┌─────────────────────┐
   │ SshSession::connect │
   │ for the selected    │
   │ server               │
   └──────────┬──────────┘
              │
              V
3. Execute
   ┌─────────────────────┐
   │ execute /            │
   │ execute_streaming /  │
   │ open_pty              │
   │ Capture output       │
   └──────────┬──────────┘
              │
              V
4. Return Results
   ┌─────────────────────┐
   │ stdout/stderr       │
   │ exit code           │
   │ (a signal-killed    │
   │  command has no exit│
   │  code -- treated as │
   │  failure, never as  │
   │  success)           │
   └─────────────────────┘
```

## Command Layer

There is no `DeploymentOrchestrator`/`*Service` class hierarchy; each
concern is a module of free functions in `jiji-cli`, called directly from a
command's `run()`.

```
commands/deploy.rs, commands/service/{restart,rollback}.rs
    │
    ├─> deploy_transaction::deploy_endpoint   (shared zero-downtime primitive)
    │     ├── mounts.rs            stage volumes/bind mounts
    │     ├── env_resolution.rs    resolve + upload .env, never inline -e
    │     ├── container_runtime.rs build/run candidate container
    │     ├── health_check.rs      health-check the candidate directly
    │     ├── service_network.rs   prepare_cutover / commit_after_health_check
    │     └── proxy_routes.rs      activate/verify kamal-proxy routes
    │
    ├─> registry.rs        resolve image references, registry auth
    │
    └─> audit::record_endpoints_by_server   append the outcome to the trail
```

**Per-command responsibilities** (see [Key Files](../CLAUDE.md#key-files) in
CLAUDE.md for exact file paths):

- **`deploy.rs` / `service/restart.rs` / `service/rollback.rs`**: build/pull
  or resolve an image, deploy with zero downtime, health check, VIP cutover.
- **`service/remove.rs`**: stop/remove both backend slots, remove proxy
  routes, deactivate the VIP/NAT mapping, optionally remove named volumes.
- **`service/prune.rs`**: list image tags per server, keep the configured
  `retain` count, remove the rest unless still referenced by a container.
- **`proxy.rs`**: install/restart/multi-home the shared kamal-proxy
  container; `proxy_routes.rs` manages its per-project routes.
- **`service/logs.rs` / `proxy` logs / `audit` reads**: tail or follow
  container/audit logs on selected hosts.
- **`audit.rs`**: append-only JSONL trail writer/reader (see
  [Data Flow](#data-flow)).
- **`lock.rs`**: per-project deployment lock file, checked before `jiji
  deploy` makes any change.

## Data Flow

### Deployment Data Flow

```
User Command
    │
    V
┌─────────────────┐
│ Configuration   │ <── Load from YAML
└────────┬────────┘
         │
         V
┌─────────────────┐
│ Network Plan    │ <── NetworkPlanner::plan (deterministic, config only)
└────────┬────────┘
         │
         V
┌─────────────────┐
│ Deployment Lock │ <── Acquire per-project lock on selected servers
└────────┬────────┘
         │
         V
┌─────────────────┐
│ SSH Connections │ <── Establish to all selected hosts
└────────┬────────┘
         │
         V
┌─────────────────┐
│ Build Images    │ <── Local or remote (jiji build / deploy --build)
└────────┬────────┘
         │
         V
┌─────────────────┐
│ Push to Registry│ <── Docker Hub, GHCR, or local loopback registry
└────────┬────────┘
         │
         V
┌─────────────────┐
│ Deploy Candidate│ <── Pull, create, health check on inactive slot
└────────┬────────┘
         │
         V
┌─────────────────┐
│ VIP Cutover +   │ <── nftables DNAT flip, then kamal-proxy route
│ Proxy Routing   │     activation/verification
└────────┬────────┘
         │
         V
┌─────────────────┐
│ Cleanup         │ <── Stop/remove previous slot's container
└────────┬────────┘
         │
         V
┌─────────────────┐
│ Release Lock +  │ <── Record outcome in .jiji/{project}/audit.log
│ Audit Log       │
└─────────────────┘
```

## Security Model

### Authentication & Authorization

**SSH Authentication**:

- SSH keys (preferred)
- SSH agent
- Private key files, inline key data
- ProxyJump/ProxyCommand for bastion hosts

**Registry Authentication**:

- Username/password
- Personal access tokens
- Environment variable substitution for secrets

**Server Access**:

- Requires sudo for system operations
- Container operations via Docker/Podman CLI
- Firewall configuration requires root

### Network Security

**WireGuard Encryption**:

- All inter-server traffic encrypted
- Public key cryptography (Curve25519)
- Perfect forward secrecy

**Firewall Rules**:

- Only required ports opened
- WireGuard: UDP, one port per project in `51820..=55819` (not a single
  fixed port) between configured server public IPs
- HTTP/HTTPS: TCP 80/443

**Container Isolation**:

- Containers isolated in their project's private network
- Not directly accessible from internet
- Exposed only through kamal-proxy or explicit port mappings

### Secret Management

**Environment Variables**:

- Secrets loaded from `.env` files in project root
- Never stored in plain text in config files
- Variable syntax: `VAR_NAME` (ALL_CAPS pattern)
- File priority: `.env.{environment}` > `.env`
- Optional host env fallback with `--host-env` flag
- Secrets are uploaded to remote hosts via a staged `--env-file`, never
  inlined into a logged `-e KEY=VALUE` command

**SSH Keys**:

- Private keys never leave local machine
- Public keys stored on servers
- Keys can be password protected

**Registry Credentials**:

- Configured in `.jiji/deploy.yml` under `builder.registry`
- Password can be a secret name (ALL_CAPS) or literal value
- Registry authentication performed locally and on remote servers

**External Secret Adapters** (e.g. Doppler): schema-only today, not wired
into any resolution path yet -- see [Follow-Up Items](followup.md).
