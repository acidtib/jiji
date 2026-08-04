# AGENTS.md

## Workspace Structure

This is a Cargo workspace with seven crates in `crates/`:

```
crates/
├── jiji-core/    # Shared primitives: pattern matching, error types, default CIDRs
├── jiji-tui/     # Terminal UI helpers (Ui::say/section/success/progress/result_ok
│                 # /result_warn/result_error/panel/confirm/confirm_typed/spinner)
├── jiji-config/  # Config schema, YAML loading, validation (jiji.yml reference lives here)
├── jiji-network/ # Deterministic identity/topology naming and per-project addressing
│                 # (NetworkPlanner, NetworkPlan, naming.rs) -- no longer computes VIPs,
│                 # backend slots, or service NAT; see "Distributed Control Plane" below.
├── jiji-ssh/     # SSH abstraction over russh (SshSession, SshPool)
├── jiji-agent/   # The project-scoped `jiji-agent` binary/library installed on every
│                 # server: durable local store, replicated membership/catalog, incremental
│                 # WireGuard repair, distributed DNS, container reconciliation.
└── jiji-cli/     # The `jiji` binary: commands, orchestration, everything else
```

Phase 0's `jiji-control-plane` executable spike was removed once `jiji-agent`
shipped its production implementation of the same invariants; it was never
wired into the CLI or installed on any host.

`jiji-cli` produces two binaries: `jiji` (declared via the single `[[bin]]`
entry in `crates/jiji-cli/Cargo.toml`, path `src/main.rs`) and `jiji_dev`
(auto-discovered by Cargo from `src/bin/jiji_dev.rs`, no `Cargo.toml` entry
needed — a separate debug binary for iterating locally without overwriting
an installed `jiji`).

## Quick Start Commands

```bash
# Build / run (via mise, or plain cargo)
mise build              # cargo build
mise run                # cargo run --
mise test               # cargo test
mise fmt                # cargo fmt
mise lint               # cargo clippy --all-targets --all-features && cargo fmt --check
mise check              # cargo check
mise scan               # osv-scanner -r . (vulnerability scan, see fix-osv-finding skill)

# Version management
mise run version                    # Show current version
mise run version -- --bump          # Auto-increment patch version
mise run version -- --bump 1.0.0    # Set specific version (updates workspace.package.version)

# Install locally
mise install             # cargo build --release --bin jiji -> ~/.local/bin/jiji
mise install-dev         # cargo build --bin jiji_dev -> ~/.local/bin/jiji_dev

# Single test file / crate
cargo test -p jiji-cli --test deploy_test
cargo test -p jiji-network
```

## High-Level Architecture

### Command Flow (CLI)

The CLI uses `clap` (derive) for command routing (`crates/jiji-cli/src/cli.rs`
defines `Cli`, `Commands`, `ServerCommands`, `NetworkCommands`). There is no
shared `setupCommandContext()`-style helper; each command's `run()` in
`crates/jiji-cli/src/commands/` repeats the same sequence inline:

```
load_config() -> validate_config() -> build NetworkPlan (if needed) ->
select hosts (NetworkPlan::select_hosts) -> connect via SshPool -> execute -> close sessions
```

`NetworkPlan::select_hosts` matches `-H`/`--hosts` filters against both a
server's config key name and its `host:` address (`ServerPlan.public_host`)
— `-H app1` matches a server named `app1` regardless of its `host:` value,
and also matches any server whose `host:` address is literally `app1`. A
filter matching a server on both counts still selects it only once.

`crates/jiji-cli/src/lib.rs::run()` is the shared entrypoint for both the
`jiji` and `jiji_dev` binaries; it dispatches on `Commands`/`ServerCommands`/
`NetworkCommands` and prints a consistent error shape for every command.

### CLI Output Conventions

All command output goes through `jiji-tui`'s `Ui` helpers
(`crates/jiji-tui/src/lib.rs`), not ad-hoc `println!`, so new commands stay
visually consistent with existing ones:

- Section headings via `Ui::section`; long-running operations report
  per-resource progress via `Ui::progress`/`result_ok`/`result_warn`/
  `result_error` as stable `OK`/`SKIP`/`FAIL` rows, not free-form logging.
- Successful completion ends with `Ui::success_elapsed` (a green `Done`
  marker plus total runtime); warnings use `Ui::warn` (`Warning:`, yellow);
  errors use `Ui::error` (`Error:`, red).
- ANSI styling is disabled automatically when output isn't a real TTY
  (redirected, captured, or piped) — never gate this manually per command.
- Raw engine/remote command details stay verbose-only; default output
  describes intent and outcome, not the underlying shell commands.

### Configuration System

`jiji-config` (`crates/jiji-config/src/schema.rs`) defines the full config
schema as plain `serde`-deserializable structs (`Config`, `NamedServer`, `Ssh`,
`Service`, `ProxyConfig`, `MountConfig`, etc.) — no lazy-loaded getters, no
`BaseConfiguration` base class. `load_config()` searches upward from cwd for
`.jiji/deploy.yml` or `jiji.{environment}.yml`. `validate_config()` returns a
`ValidationResult` with explicit errors, not a throw-on-first-error model.

`crates/jiji-config/src/jiji.yml` is the authoritative configuration reference
(same file `jiji init` writes as a template).

### Distributed Control Plane

Jiji replaced its original compiled, all-host `NetworkPlan` design with a
**distributed, per-project control plane**: every server runs a project-scoped
`jiji-agent` (`crates/jiji-agent/`) that owns a durable local store, replicates
signed membership and a signed service catalog peer-to-peer over WireGuard (no
CLI-driven SSH fan-out to every host), incrementally repairs its own WireGuard
peers, serves project DNS from its local catalog, and reconciles its own
containers/leases/proxy routes on restart. There is no legacy runtime or
mixed-version cluster to support: protocol and schema versions are checked
before any state exchange (`jiji-agent/src/membership.rs`, `replication.rs`,
`catalog_replication.rs`), and a mismatch is rejected outright rather than
partially applied.

Non-negotiable invariants: a service deployment never rewrites WireGuard; a
targeted deploy normally connects to and locks only its affected host and
logical replica; temporary absence of a host or peer never means permanent
deletion, only an authenticated tombstone does; DNS only ever publishes an
`active`+`healthy` catalog record.

Supported capacity per project (`crates/jiji-config/src/validation.rs`):
32 nodes in a full WireGuard mesh, 500 services, 2,000 logical replicas.
Configuration validation rejects node/service/replica cardinality above these
limits outright.

### Zero-Downtime Deployment Strategy

Jiji uses **dynamically leased deployment addresses**, not fixed dual-address
(A/B) backend slots or a stable service VIP. A **logical replica** (stable
`replica_id`, survives restart/redeploy) points at exactly one **deployment**
(the actual container underneath it, replaceable). `jiji-cli/src/placement.rs`
derives `replica_id` deterministically from `(project, service, ordinal)`; the
agent allocates a fresh address per `deployment_id` from the host's
container subnet on every deploy (`AllocateAddress`), so there is no
rename step and no fixed per-service address to collide with.

Flow (`crates/jiji-cli/src/deploy_transaction.rs::deploy_dynamic_endpoint`,
orchestrated by `commands/deploy.rs` through `crate::agent_client`'s calls
into the target host's own agent socket):

1. Read the replica's current `active`+`healthy` catalog record, if any
   (`agent_client::catalog`), and allocate a new address for a fresh
   `deployment_id` (`RequestBody::AllocateAddress`).
2. Stage mounts (`mounts.rs`) and environment (`env_resolution.rs`, via a
   remote `--env-file`, never inline `-e KEY=VALUE`, so secrets never appear
   in a logged command).
3. Commit the candidate to the catalog as `Candidate`/`Unknown`
   (`RequestBody::CatalogCommit`) *before* starting it, so its existence is
   durable even if the container start fails.
4. Create and start the candidate container
   (`container_runtime::build_dynamic_run` + `container_ops::create_and_start`)
   at its own newly leased address, named
   `container_runtime::dynamic_container_name` (`{project}-{service}-{first
   12 hex chars of deployment_id}`) — the previous deployment keeps serving
   traffic throughout.
5. Health-check the candidate directly at its own address (`health_check.rs`),
   never through the proxy or a VIP.
6. Activate/verify kamal-proxy routes (`proxy_routes.rs`) at the candidate's
   address; roll back (remove the candidate, restore the previous route) if
   that fails. The rendered `kamal-proxy deploy` command always carries a
   `--health-check-timeout` (defaulted if not configured) and is itself
   wrapped in an outer `timeout --signal=TERM --kill-after=5s`, so a wedged
   kamal-proxy deploy can never hang the whole `jiji deploy` run.
7. Commit the candidate as `Active`/`Healthy`, then the previous deployment
   (if any) as `Draining`, in separate catalog transactions.
8. Stop and remove the previous deployment's container, release its address
   lease (`RequestBody::ReleaseAddress`), then commit it `Tombstoned`.

If any step through health-checking or proxy activation fails, the previous
container and route are never touched and keep serving traffic; only the
candidate is torn down. `service.stop_first: true` is a distinct transaction
that stops the previous container before starting the candidate (for
services that cannot tolerate two running instances) and attempts to restart
it if the candidate fails to come up.

### Private Networking (WireGuard mesh + agent-served DNS)

`jiji network setup` (`crates/jiji-cli/src/commands/network/setup.rs`) still
writes each host's WireGuard bootstrap material to a symlink-swapped
"generation" tree under `/etc/jiji/network/{slug}/` — but that generation now
covers **mesh bootstrap only** (the WireGuard interface, its initial peer set,
the bridge/engine network). It is a one-time/repair step, not something a
service deploy ever touches: `jiji deploy` never re-runs it except to reconcile
a genuinely stale host before continuing. Everything that changes on every
deploy or scale — membership updates after the first tunnel, the service
catalog, DNS answers, container reconciliation — is owned continuously by the
long-running `jiji-agent-{slug}.service` (`crates/jiji-agent/`, installed by
`jiji server setup`, one process per project per host), not recomputed and
reapplied by the CLI. `network setup`'s own `remove_legacy_service_runtime`
step additionally cleans up any pre-agent installation's `jiji-dns-{slug}`/
`jiji-service-nat-{slug}` systemd units and nftables table it finds on a host,
since those belonged to the old compiled-DNS/VIP-NAT design this control plane
replaced (Phase 8) and are never reinstalled.

**Per-project isolated, not a host-global singleton.** Every name and path
below is derived purely from `config.project` (`crates/jiji-network/src/
naming.rs`) — two independent projects can run `jiji server setup` against
the same physical host and get two fully independent sets of the following,
with zero shared/persisted state between them (see "Naming Conventions"
above for the exact derivation and the jiji-website repo's Network Reference
page, `~/Code/jiji-website/app/docs/reference/network/page.mdx`, for the
operator-facing explanation, including the residual hash-collision risk when
projects share default CIDR ranges):

- **WireGuard**: interface name `jiji{8 hex}`, one per project. Config is
  still written to `/etc/jiji/network/{slug}/generations/{gen}/wireguard.conf`
  (symlinked to `/etc/wireguard/{wireguard_interface}.conf`) for manual
  `wg-quick`/`wg` inspection and recovery, but as of Phase 9 no
  `wg-quick@{iface}.service` unit is installed or enabled: the agent brings
  the interface up itself at startup (`local_reconcile.rs::ensure_link`,
  equivalent to what `wg-quick` did, since jiji's rendered config never used
  `PostUp`/`PostDown` hooks) and repairs it if torn down externally.
  WireGuard port is per-project (`51820..=55819`), not the fixed `51820`.
  Once the first tunnel is up, `jiji-agent` takes over incremental peer
  repair (`jiji-agent/src/wireguard.rs::plan_reconciliation`) — new hosts
  join and endpoints roam without the CLI re-touching this generation.
- **Bridge/engine network** (`commands/network/bridge.rs`): a
  `jiji-{slug}` docker/podman network per project (kernel device name
  `jijib{7 hex}`, distinct from the logical name because of Linux's 15-char
  interface limit). As of Phase 9 the agent brings this up itself at
  startup (`local_reconcile.rs::ensure_bridge_and_dns`/`bridge_bringup.rs`)
  instead of depending on a separate `jiji-network-restore-{slug}.service`
  oneshot unit, and the Podman-only `podman-restart.service.d` drop-in
  (which used to restart `unless-stopped` containers on boot) is retired
  entirely — the agent's own post-restart container reconciliation
  (`reconcile_containers`/`recover_startup_candidates`) was already
  authoritative, so the drop-in was a straight duplicate.
- **Service catalog**: `jiji-agent` replicates a node-signed, append-only
  operation log peer-to-peer over WireGuard (`jiji-agent/src/catalog.rs`,
  `catalog_replication.rs`), authoritative for each logical replica's current
  deployment, address, image, and `Candidate`/`Active`/`Draining`/`Stopped`/
  `Tombstoned` state. There is no CLI-driven VIP/NAT cutover and no
  `service-nat` nftables table for live traffic; `jiji-cli/src/
  deploy_transaction.rs` is the only thing that ever commits new records
  in the normal deploy path.
- **DNS**: each `jiji-agent` process serves the `.jiji` zone directly from its
  local replicated catalog (`jiji-agent/src/dns.rs`, a hand-rolled minimal
  authoritative resolver on the project's management address, UDP with TCP
  fallback for large answer sets) — there is no `dnsmasq` process and no
  compiled `dns.conf` in the running system anymore. Only `active`+`healthy`
  records are ever answered with; a peer the local agent currently considers
  unreachable is suppressed reversibly, never deleted, from both the
  aggregate (`{project}-{service}.jiji`) and per-server
  (`{project}-{service}-{server}.jiji`) names.
- **kamal-proxy** (`crates/jiji-cli/src/proxy.rs`): a Go reverse proxy
  container (fork `ghcr.io/acidtib/kamal-proxy:jiji`), provisioned per-server
  by `jiji server setup`. Deliberately the **one genuinely shared,
  multi-tenant** component: one container per host, **multi-homed** across
  every project's bridge that has active routes on that host
  (`network connect --ip <ServerPlan::proxy_address> <bridge_name>
  kamal-proxy`, idempotent/additive — see `ensure_attached`), routes
  namespaced per project already. Given no `--dns` at all (unlike service
  containers): its routing targets are raw backend IPs
  (`proxy_routes::RouteTarget::address`), never a `.jiji` hostname, and a
  single resolver can't reliably answer for every attached project's `.jiji`
  records at once. `commands/server/teardown.rs` disconnects kamal-proxy from
  a project's bridge (`proxy::disconnect_bridge_if_attached`) before that
  bridge can be removed, independent of whether kamal-proxy is still running
  for other projects.

  Confirmed live on Docker: kamal-proxy's own `--publish 80:8080 --publish
  443:8443` silently drops its IPv4 binding, because its primary network is
  always one of the bridges above and those are created with
  `enable_ip_masquerade=false` + `gateway_mode_ipv4=routed` (needed for
  routable backend addresses across the WireGuard mesh) — dockerd logs "Host
  port ignored, because NAT is disabled" and the IPv6 publish keeps working
  while IPv4 silently doesn't. `crates/jiji-cli/src/proxy_ingress.rs` works
  around this Docker-only (Podman's bridge creation doesn't set either
  option, so it's unaffected): a host-global (not per-project, since
  kamal-proxy itself is host-global) nftables table
  (`jiji_proxy_ingress`, `/etc/jiji/proxy-ingress/`) DNATs the public ports
  straight to kamal-proxy's bridge address, bypassing Docker's own
  port-publish path entirely. As of Phase 9, ingress reconciliation is
  owned by whichever co-resident project's agent currently holds a
  same-host, non-blocking `flock` lease
  (`crates/jiji-agent/src/host_lease.rs`,
  `/etc/jiji/proxy-ingress/agent.lock`) — no separate
  `jiji-proxy-ingress-restore.service` boot-persistence unit exists
  anymore; the lease holder's agent reapplies the nftables table on every
  reconcile tick (`proxy_bringup.rs`). `ensure_proxy` (CLI) still
  re-applies it idempotently on first install;
  `proxy_teardown::teardown_proxy_container_if_unused`
  removes it only when kamal-proxy's own container is finally removed (no
  project has routes left).

Docker/Podman's own IPAM has no knowledge of jiji's reserved infrastructure
addresses (`ServerPlan::dns_address`, `proxy_address`, `bridge_gateway`) or of
whatever deployment addresses the agent has currently leased: `jiji-agent`
runs as a host-level systemd process, not a container, so the engine can and
will hand out `dns_address` to an ad-hoc container started on a jiji bridge
without an explicit `--ip` (confirmed live, pre-isolation, against the old
shared `jiji` bridge: `docker run --network jiji nginx:alpine` got assigned
the DNS address and silently broke resolution for that container — the same
risk applies to any project's `jiji-{slug}` bridge today). Every jiji-managed
container avoids this because `container_runtime`/`proxy.rs` always pin
`--ip` explicitly to an address `jiji-agent` itself leased
(`leases.rs::AddressAllocator`) — any new code that runs a container on a
jiji bridge (debug tooling, health-check sidecars, etc.) must do the same.

### Container Namespace Sharing (`network_mode: service:<name>`)

A service can share another ("upstream") service's container network
namespace instead of getting its own dynamically-leased bridge address, via
`network_mode: "service:<upstream-name>"` (Compose's shorthand for what
Docker/Podman render as `--network container:<name>`) — the standard "VPN
killswitch" pattern, where a torrent client shares a VPN gateway container's
network stack so all its traffic is forced through the tunnel. Naming the
upstream this way is itself the dependency declaration; there is no separate
`depends_on` field. `jiji_config::Service::network_mode_dependency()` parses
it; `validation.rs` rejects an undefined/self-referencing upstream, a
chained dependency (the upstream must itself use `network_mode: bridge` —
v1 supports exactly one level, not chains), a `servers:` list that isn't a
subset of the upstream's, and (via the pre-existing `NON_BRIDGE_SCALE`/
`NON_BRIDGE_PROXY` rules, which already generalize to any non-`"bridge"`
value) `replicas` above 1 or a `proxy:` block of its own — a dependent is
reached through the upstream's own route, at the upstream's own address,
never a route of its own.

A dependent has no address to lease: `deploy_transaction.rs::
deploy_shared_endpoint` (a sibling of the normal `deploy_dynamic_endpoint`,
dispatched from `deploy_endpoint` based on `network_mode_dependency()`)
skips `AllocateAddress`/`ReleaseAddress` entirely, resolves the upstream's
current Active/Healthy catalog record by filtering on `service` + `owner_
node_id` (not by recomputing a replica_id through placement arithmetic —
the upstream may use a different placement policy, but at most one of its
replicas can ever be Active/Healthy on a given server), and runs
`container_runtime::build_shared_run` /
`NetworkedContainerRun::shared` (`--network container:<upstream's current
container name>`, no `--ip`/`--dns*`/`-p` — all owned by the upstream).
Since a dependent can't configure `healthcheck:` (no `proxy:` allowed),
it gets `health_check::plan_for_candidate`'s existing no-config fallback:
an engine-native container-readiness check, the same one any bridge
service without an explicit `healthcheck:` already gets.

`commands/deploy.rs::add_cascaded_dependents` automatically adds every
direct dependent of a selected upstream to the deployment (visible in the
printed plan/confirmation prompt), using `placement::endpoint_replica_id`
(sorted-position-in-`servers` ordinal) rather than `placement::place` for
the dependent's own replica_id — a dependent's real cardinality is "one
instance per shared-namespace server," not an independently round-robined
replica count. Selecting a dependent alone never cascades the other
direction: it just attaches to the upstream's already-existing deployment,
failing actionably if the upstream has none.

The upstream's own redeploy must complete (its old container is only torn
down as part of that same transaction) before any cascaded dependent can
attach to its new one, so `commands/deploy.rs` deploys in two sequential
waves (an upstream-with-dependents wave, then a dependents wave) rather than
the usual single `SshPool::execute_concurrent` call — an in-closure wait on
the upstream's completion would deadlock, since `execute_concurrent`
acquires its semaphore permit *before* running a task, and the pool is
bounded to 1 whenever any selected service configures `proxy:` (true for a
VPN-gateway-shaped upstream). If the upstream's own proxy route targets a
port only a dependent actually serves, its inline `activate_proxy_routes`
call is forced to defer (`skip_proxy: true`) whenever it has a dependent in
the second wave; the route is instead verified by the already-existing
`reconcile_catalog_routes` pass, which already runs once after every
selected endpoint — upstream and dependents alike — has finished.

This is not itself zero-downtime with respect to upstream churn: there is a
real gap between the upstream's old container being removed and each
dependent's own redeploy completing, during which an old dependent
container may be degraded — matching Compose's own `depends_on: {condition:
service_healthy, restart: true}` semantics for this exact pattern (two
containers can't simultaneously share one upstream's single network
namespace during a bridge-swap-style cutover).

### `jiji server teardown` (inverse of `server setup`)

`crates/jiji-cli/src/commands/server/teardown.rs` orchestrates the inverse of
everything above, holding a `HostRuntime` lock per targeted host
(`crate::lock::LockScope::HostRuntime`, see "Deployment Locking" below) for
the duration: proxy routes -> application containers -> volumes (with
`--volumes`) -> images -> the shared kamal-proxy container (only when no
project still has routes) -> disconnecting kamal-proxy from this project's
bridge (`proxy::disconnect_bridge_if_attached`, independent of whether
kamal-proxy is still running for other projects) -> the per-project staging
directory (`env_resolution::project_staging_dir`, holds staged `.env` files
with resolved secrets and uploaded mount content) -> (only once every
application-layer step above succeeded) this project's own network layer:
WireGuard, any legacy pre-agent nftables table, bridge
network, compiled `/etc/jiji/network/{slug}` subtree only (never a sibling
project's subtree on a shared host) -> finally `jiji-agent` itself (as of
Phase 9 the only jiji-authored systemd unit this project ever installs)
(`agent_install::remove_agent`). Ownership discovery is by
`jiji.managed`/`jiji.project` labels for containers, and config-computed exact
names (never a glob) for volumes/images/proxy routes. `-S`/`--services` is
explicitly rejected rather than silently ignored. Another project's
containers being present on the same host is surfaced as an informational
notice (`teardown_plan::render_other_project_notices`), not a blocker —
teardown only ever acts on this project's own labeled resources.

### SSH Connection Management

`jiji-ssh` is built on **russh** (pure-Rust async SSH client, no subprocess,
no libssh FFI). `SshSession::execute`/`execute_with_input` enforce
`connect_timeout`/`command_timeout`. `SshPool` (semaphore-based) provides
`execute_concurrent`/`execute_batched`/`execute_with_error_collection` for
running independent SSH operations across many hosts without overloading any
one server. `SshSession` supports stream-backed nested sessions for ProxyJump
and loopback-bound reverse TCP forwarding with explicit cancellation.
`crates/jiji-cli/src/ssh_adapter.rs` resolves supported OpenSSH config fields
and adapts `jiji_config::{NamedServer, Ssh}` into `jiji_ssh::ConnectOptions`.

A remote command killed by a signal never sends `ChannelMsg::ExitStatus` (SSH
sends `exit-signal` instead, which `classify_channel_msg` in `session.rs`
ignores) — so `Option<u32>` exit codes must treat `None` the same as a
nonzero exit, never as success. `run_command`'s `success: code == Some(0)`
gets this right; `commands/server/exec.rs`'s streaming/PTY paths originally
didn't (`Some(0) | None => Ok(())`), silently reporting success on a killed
remote command, fixed to bail on `None` with an actionable message.

### Deployment Locking

`crates/jiji-cli/src/lock.rs` generalizes CLI coordination to match mutation
scope (Phase 7): a mkdir-atomic SSH lock primitive
(pending dir -> `install` an `info.json` -> `mv -T` into place) taken over
`LockScope`, a fixed, deadlock-free rank order:

```
ProjectMaintenance (0) < HostRuntime (1) < ServiceScale (2)
  < LogicalReplica (3) < HostGlobalProxy (4)
```

A command computes its full lock set up front, sorts by `(rank, host, path)`,
and acquires concurrently within a rank, sequentially rank-to-rank -- since
every command's lock set is a subset of one fixed total order, two commands
can never deadlock against each other. `ProjectMaintenance` covers `network
setup`/`backup`/`restore`/`import`/`compact`; `HostRuntime` covers `server
setup`/`teardown`; `ServiceScale` is keyed by service name so unrelated
services scale independently; `LogicalReplica` (keyed by `replica_id`) is
what `deploy`/`service restart`/`rollback`/`remove` actually lock, so an
unrelated offline host or a different replica never blocks a targeted
operation; `HostGlobalProxy` is the one host-global (no project segment)
lock, since kamal-proxy itself is host-global and shared across projects.
`commands/lock/mod.rs::with_locks` acquires/releases over a caller's
already-connected session map instead of opening a dedicated connection
episode per command, and `crate::agent_client`/deploy sessions are reused for
the lock the same way. See "Command Reference" above for the `jiji lock`
CLI surface this backs.

### Naming Conventions

- **Images**: explicit `image:` references, or versioned references produced by
  `jiji build` / `jiji deploy --build` from each service's `build:` config.
  Static short names are normalized before engine operations:
  `nginx:latest` becomes `docker.io/library/nginx:latest`, and
  `owner/image:tag` becomes `docker.io/owner/image:tag`; references whose
  first path component is `localhost`, contains a dot, or contains a port
  remain unchanged.
- **Logical replicas**: `placement::replica_id(project, service, ordinal)`
  (`crates/jiji-cli/src/placement.rs`) — a stable ID surviving redeploy,
  independent of which host currently owns it; deterministic per
  `(project, service, ordinal)`, not derived from a container name.
- **Deployments**: a fresh, random `deployment_id` per container start
  (`deploy_transaction.rs::deploy_dynamic_endpoint`, hashed from project,
  replica ID, a nonce, and the CLI process ID).
- **Containers**: `{project}-{service}-{first 12 hex chars of deployment_id}`
  (`container_runtime::dynamic_container_name`) — a new name every deploy, no
  permanent A/B slot names, no rename step.
- **Proxy targets**: `{project}-{service}-{port}` (per-server local route
  name in that server's own kamal-proxy — routes are not shared/synced across
  servers).
- **DNS records**: `{project}-{service}.jiji` (aggregate) and
  `{project}-{service}-{server}.jiji` (per-replica), served live by each
  host's own `jiji-agent` from its local catalog, not compiled ahead of time.
- **Ownership labels**: `jiji.managed=true jiji.project=<p> jiji.service=<s>
  jiji.server=<srv> jiji.resource=service` on every service container.
- **Per-project network identifiers** (`crates/jiji-network/src/naming.rs`,
  all pure functions of `project:` alone, computed the same way independent
  of whether any other project shares the host — see the jiji-website repo's
  Network Reference page's "Multiple projects on one server" section):
  WireGuard interface `jiji{8 hex}` (`wireguard_interface_name`), kernel
  bridge device `jijib{7 hex}` (`bridge_interface_name`, distinct from the
  logical bridge name below because Linux interface names are capped at 15
  characters), Docker/Podman logical network `jiji-{slug}`
  (`bridge_network_name`), the sole per-project systemd unit
  `jiji-agent-{slug}.service` (`systemd_unit_slug`,
  `jiji-agent/src/paths.rs::unit_name`), WireGuard port `51820..=55819`
  (`wireguard_port`), the agent's membership/catalog replication TCP ports
  `56000..58000` / `58000..60000` (`membership_replication_port`,
  `catalog_replication_port`). All remote state lives under
  `/etc/jiji/network/{slug}/` (mesh bootstrap) and `/etc/jiji/agent/{slug}/`
  (agent binary, durable store, socket — `AgentPaths::default_for_project`)
  instead of one shared top-level path. `jiji-dns-{slug}.service` /
  `jiji-service-nat-{slug}.service` / `service_nat_table_name` still exist as
  named constants purely so `network setup`/`teardown` can find and remove
  them from a pre-agent installation (see "Private Networking" above) — no
  current code path (re)installs them.

## Key Files

- `crates/jiji-config/src/jiji.yml` — authoritative configuration reference
  (all options), also the template `jiji init` writes.
- `crates/jiji-config/src/schema.rs` — the full config schema.
- `crates/jiji-network/src/planner.rs` — `NetworkPlanner`/`NetworkPlan`, the
  deterministic per-server addressing/topology computation (WireGuard peers,
  container subnets, reserved infrastructure addresses) that mesh bootstrap
  still needs; carries no service/deployment state.
- `crates/jiji-network/src/naming.rs` — every project-derived name (WireGuard
  interface/port, bridge interface/network name, systemd unit slug,
  replication ports); the single source of truth the per-project isolation
  design depends on.
- `crates/jiji-cli/src/placement.rs` — `replica_id`/`endpoint_replica_id`,
  deterministic initial placement across a service's eligible servers.
- `crates/jiji-cli/src/deploy_transaction.rs` — the dynamic per-endpoint
  deploy transaction (`deploy_dynamic_endpoint`): lease, candidate commit,
  start, health-check, proxy activation, active/draining/tombstoned catalog
  commits.
- `crates/jiji-cli/src/lock.rs` — the ranked, scope-aware CLI lock primitive
  (`LockScope`, `LockRequest`, `OwnedDeploymentLocks`) described under
  "Deployment Locking" below.
- `crates/jiji-agent/src/store.rs` — the durable local `AgentStore` (SQLite),
  the single source of truth every replicated/local-only structure below is
  persisted through.
- `crates/jiji-agent/src/membership.rs`, `catalog.rs` — the signed record
  types (`MembershipRecord`, `CatalogRecord`) and their validation rules;
  `replication.rs`/`catalog_replication.rs` are the peer-to-peer anti-entropy
  exchanges that carry them between hosts.
- `crates/jiji-agent/src/runtime.rs` — wires replication, DNS, and
  WireGuard repair together into the agent's continuous run loop.
- `crates/jiji-agent/src/local_reconcile.rs` — autonomous repair of local
  runtime state (bridges, routes, DNS binding, proxy attachment, container
  discovery) from durable catalog records plus local observations.
- `crates/jiji-agent/src/dns.rs` — the hand-rolled `.jiji` zone resolver
  served from the local replicated catalog.
- `crates/jiji-agent/src/leases.rs` — `AddressAllocator`, the durable
  per-deployment address lease/quarantine/recovery logic.

### Container Engine Provisioning

`engine::ensure_engine` (`crates/jiji-cli/src/engine.rs`) is shared by
`jiji server setup` and `builder.remote` preflight. On Debian/Ubuntu, Podman
is installed as a pinned, SHA256-verified static binary
(`mgoltzsche/podman-static` v5.8.4) rather than the distro package, because
those distros don't ship a Podman new enough for current CDI specs (min
version 5.8.4, up from the old distro-packaged 4.9.3 floor); Fedora/RHEL
still get `dnf install podman`. The static install also writes a managed
`/etc/containers/containers.conf.d/99-jiji-static.conf` pinning
`runtime = "/usr/local/bin/crun"` / `cgroup_manager = "cgroupfs"`, and patches
the AppArmor `podman` profile to permit `/usr/local/bin/podman`. Unlike
Docker (which still requires an operator-managed upgrade), an
already-installed Podman below the minimum is upgraded in place —
`reconcile_managed_podman_static_configuration` re-applies the pinned config
on every `ensure_engine` call so a manual Podman upgrade never drifts from
it. Every exec into a container (`container_runtime::exec_prefix`) also
passes Podman `--no-session` (Docker needs no equivalent flag), avoiding the
overhead of a new PAM/login session on every health-check poll and proxy
command.

## Command Reference

- `jiji init` — scaffolds `.jiji/deploy.yml`, including project-directory-derived
  `/24` management and `/16` container CIDRs persisted in the generated config.
- `jiji server setup` — container engine install (Docker/Podman version
  check, distro-aware install), then `jiji-agent` install-and-start (the
  agent brings up WireGuard/bridge/DNS and kamal-proxy/ingress itself at
  startup, as of Phase 9 -- see "Private Networking" above), in that order,
  all under one `HostRuntime` lock per targeted host.
- `jiji network plan` / `jiji network setup` — print or transactionally apply
  the deterministic mesh-bootstrap plan (idempotent, rollback on partial
  failure; WireGuard peers, bridge, reserved infrastructure addresses only —
  no service/deployment state, see "Distributed Control Plane" above). Setup
  also migrates an existing project bridge when configured CIDRs change: it
  detaches only that project's currently running containers and the shared
  proxy attachment, recreates the bridge, reattaches them at their new
  planned addresses, refreshes ingress/routes, and restores the previous
  bridge and addresses if activation fails. `jiji server setup` uses this
  same path. The network layer is per-project isolated (own WireGuard
  interface/port, bridge, agent, compiled bootstrap tree per project — see
  "Private Networking" above), so multiple independent projects can share
  one server; kamal-proxy is the one intentionally shared, multi-homed
  component.
- `jiji network catalog` / `diagnostics [--json]` — read-only inspection of a
  selected host's locally replicated service catalog, or of its agent's
  self-healing/replication/quota/component diagnostics.
- `jiji network decommission` / `update-endpoint` / `rotate-key` / `replace`
  / `rotate-authority` — publish signed membership changes (tombstone a
  node, publish a changed public endpoint, rotate or replace a node's
  WireGuard transport key, rotate the project membership authority) through
  one reachable seed. Share their publish/signing logic through
  `commands/network/membership.rs`, which is not itself a CLI verb.
- `jiji network backup --output --passphrase-file` / `restore` / `recover` —
  export an encrypted operator-controlled control-plane backup (project
  identity, membership, address claims, catalog operations; never host
  WireGuard private keys or deployed secrets), restore it into surviving
  hosts in the same recovery epoch, or recover a lost control plane into a
  new fenced epoch (`recover` requires `--yes`, a destructive epoch advance).
- `jiji network compact` — compacts each selected host's superseded
  replicated operation history.
- `jiji network assess` — read-only comparison of a host's current resources
  (labeled containers, membership, catalog records, address capacity)
  against the distributed control plane, for operators deciding between
  clean teardown+setup and `jiji network import` (Phase 8). Never mutates
  anything.
- `jiji network import [--dry-run] [-y]` — one-way seeding of catalog history
  (as `Stopped` records) from containers discovered on a stopped old
  installation, so `jiji service`/`jiji network catalog` show continuity
  instead of a blank slate. Operator convenience, not a compatibility layer:
  never marks anything `Active`, never allocates a lease, and never touches
  a replica whose existing catalog record is already live. Requires the
  target host's agent to already be running (`jiji server setup` first) and
  holds a `ProjectMaintenance` lock while committing.
- `jiji deploy` — full zero-downtime deploy (see "Zero-Downtime Deployment
  Strategy" above): mounts, env/secrets, dynamic address leasing, health
  checks, kamal-proxy routing, `-H`/`-S` filtering, `stop_first`, optional
  image builds, and automatic mesh reconciliation only when a targeted host
  is actually stale. Locks only the selected logical replicas (plus a
  host-global proxy lock on ingress hosts, see "Deployment Locking" below).
  Prints the deployment plan (project, environment, target servers/
  endpoints, build/version/proxy flags) and prompts for confirmation before
  touching anything -- build, mesh reconciliation, and SSH connections all
  happen after confirmation, not before. `-y`/`--yes` skips the prompt;
  without it and without a real terminal attached (no TTY on stdin/stdout,
  e.g. CI/CD), `confirm_deployment_plan` (`commands/deploy.rs`) bails
  immediately with an actionable error rather than hanging on an
  unanswerable prompt. `--wait-for-peers <N>` optionally, non-blockingly
  checks up to N other peers' catalogs for the new deployment after success
  and reports a bounded acknowledgment summary; it never affects the exit
  code or waits past a short deadline.
- `jiji build` and `jiji deploy --build` — local Docker/Podman builds,
  multi-architecture publishing, remote registries, and a loopback-only local
  registry exposed to deployment hosts through temporary reverse SSH tunnels.
  `builder.remote` (`ssh://[user@]host[:port]`, parsed by
  `jiji-config/src/remote_builder.rs`) runs the build itself on a dedicated
  remote host instead of the local engine: `jiji-cli/src/remote_build.rs`
  connects over SSH, preflights the remote engine (installing
  `builder.engine` there via `engine::ensure_engine` -- the same
  distro-aware installer `jiji server setup` uses -- if it's missing,
  surfaced with an "installed on ..." status line; only the multi-arch
  tooling a build actually needs, Buildx or `podman manifest`, stays
  detect-and-report only, not installed), stages the build context, streams
  the build/push commands, and cleans up its staging directory on every
  exit path (mirrors `build_engine.rs`'s local command rendering, just
  driven remotely).
- `jiji registry login` / `jiji registry logout` — authenticate or clear
  credentials on the local machine and/or `-H`-selected servers
  (`--skip-local`/`--skip-remote`), password delivered over stdin only,
  idempotent logout, local-registry no-op, per-target result aggregation.
- `jiji registry teardown` — validates ownership and configured port before
  removing the exact local `jiji-registry` container, with `--dry-run` and
  typed or `--yes` confirmation.
- `jiji server teardown` — full inverse of `server setup` (see architecture
  section above), including `--dry-run`, `--volumes`, typed project-name
  confirmation.
- `jiji server exec` — runs a command on any number of `-H`-resolved
  servers (concurrently by default, one at a time with `--sequential`), or
  attaches an interactive login shell or `--interactive` PTY to exactly one.
  A PTY is bound to one local terminal, so an interactive session (no
  command, or `--interactive`) requires `-H` to resolve to a single server;
  a plain command doesn't. Local raw-mode terminal handling and resize
  forwarding (`SIGWINCH` -> `channel.window_change`) live in
  `commands/server/exec.rs`, not `jiji-ssh`. Automatically downgrades to
  non-interactive when stdin/stdout aren't a real TTY, with a warning.
  `-S`/`--services` is rejected.
- `jiji-ssh` — connect, auth (key files, inline key data, ssh-agent
  fallback), DNS resolution with retry/backoff (`ssh.dns_retries`),
  `ProxyCommand` (`ssh.proxy_command` or `~/.ssh/config`, spawned as the
  first hop's transport, mutually exclusive with ProxyJump on the same
  server), `execute`/`execute_with_input`, `execute_streaming`/
  `execute_streaming_with_input` (stdout/stderr/exit delivered as they
  arrive over an `mpsc::Receiver`), `open_pty` (PTY/interactive-exec
  channels, driven by `jiji server exec`), `sftp_put`/`sftp_get` (via
  `russh-sftp`, no CLI command consumes this yet — it stays a standalone
  primitive rather than replacing `mounts.rs`'s existing upload path),
  pooled concurrent execution.
- `jiji secrets print` — non-fatal `.env`/host-env resolution status
  (`[SET]`/`[MISSING]`, `--show-values` to reveal, `-S` to filter services)
  for every secret-shaped reference in configuration. Only
  `environment.secrets` (project + per-service) and `builder.registry.password`
  are actually resolved from `.env`/host-env by any runtime code path;
  `ssh.key_passphrase`, `servers.*.host`, proxy SSL certs, build
  args, and `${VAR}` command interpolation are scanned and reported for
  visibility only, since the current SSH/proxy/build code paths use those
  fields as literal values with no env-var-reference resolution at all —
  `commands/secrets/print.rs` flags this gap explicitly in its own output
  rather than implying those fields resolve when they don't.
- `jiji proxy restart` / `jiji proxy logs` — unconditionally re-pull and
  recreate the shared per-host kamal-proxy container, or read its logs from
  selected hosts (`--follow` requires exactly one). Restart preserves the
  named configuration volume and bypasses the config-fingerprint no-op so a
  changed moving image tag is picked up. Log filters are shell-quoted.
- `jiji service logs/restart/rollback/remove/prune/scale` — singular
  `service`, not `services`. `logs` tails the currently active deployment's
  container per selected endpoint (`--container-id` bypasses catalog
  resolution entirely for an arbitrary container name; `--follow` requires
  exactly one target), sharing its command-rendering and streaming code with
  `proxy logs`. `restart` is a zero-downtime replacement cycle built
  directly on the same `deploy_endpoint` primitive `jiji deploy` uses (fresh
  lease, health check, catalog/proxy cutover, previous-deployment cleanup),
  reusing `service.image` when set or otherwise discovering the currently
  running image by inspecting the active container (for build-only services
  with no static `image:`). `rollback` is the same `deploy_endpoint` cycle
  but for a caller-supplied `--version` (required) instead of whatever is
  currently running: a build-configured service resolves the target purely
  from `builder.registry` + project + service name (no rebuild, no
  per-endpoint SSH round trip, trusting the tag was already pushed by a
  prior `jiji build`/`jiji deploy --build`); a static-`image:` service gets
  `--version` applied the same way `jiji deploy --version` does, and is
  rejected the same way if the image already carries an explicit tag.
  `remove` discovers each selected endpoint's actually-owned, non-terminal
  `replica_id`(s) first, locks exactly those (per-replica, not a coarser
  whole-service lock), then stops/removes their containers, removes any
  proxy routes, and tombstones the catalog record; `--volumes` additionally
  removes the selected services' named volumes. `prune` implements the
  `service.retain` pruning: lists each build-configured service's image tags
  per server (trusting the engine's own newest-first `images` ordering
  rather than parsing `CreatedAt`), keeps the first `retain` (or `--retain`
  override), and removes the rest unless still referenced by a container.
  Services with only a static `image:` (no `build:`) are never pruned;
  `prune` is deliberately left unlocked (its targets are already terminal
  catalog rows). `scale -S <service> --replicas N` (or `--reset` to drop a
  runtime override back to the configured count) writes a distributed
  desired-scale override under one `ServiceScale` lock keyed by service name,
  so two different services scale independently and two scales of the same
  service always serialize regardless of which hosts either touches.
- `jiji lock acquire/release/status/show` — scope-aware locking (see
  "Deployment Locking" below): `acquire`/`release` default to the
  project-maintenance scope; `release --replica <id>` / `--service <name>` /
  `--scope host-runtime|proxy` targets a specific stuck finer-grained lock
  instead. `status`/`show` list every lock file present on selected hosts
  (project-maintenance, host-runtime, per-service, per-replica, proxy), not
  just one. `acquire` polls up to `--timeout` seconds (default 300) waiting
  for an existing lock to clear before giving up, or `--force` to override
  immediately.
- `jiji audit` — a per-project, per-server, append-only JSONL trail at
  `.jiji/{project}/audit.log` (same staging root as the lock file and
  uploaded env files), each line one `{timestamp, action, status, actor,
  message, duration_ms, lock_scope, deployment_id}` object
  (`crates/jiji-cli/src/audit.rs`; `duration_ms`/`lock_scope`/`deployment_id`
  are all optional/additive, omitted from entries written before each field
  existed or when a call site has nothing to report -- `service_prune`'s
  entries never carry `lock_scope` since it's deliberately unlocked;
  `deployment_id` is only ever populated when a server-level entry covers
  exactly one replica/deployment). Writes are best-effort
  (`audit::record`): a failed audit write is warned, never propagated, so
  audit logging can never mask or block the outcome of the command it's
  recording. Reads are `tail -n <lines>` per selected host, with malformed
  lines silently skipped (an audit line from a future incompatible format,
  or a rare concurrent-write interleaving, must never make the trail
  unreadable). There is deliberately no `--filter`/`--since`/`--until`/
  `--raw`/`--aggregate` surface — `jiji audit` instead mirrors this
  codebase's own `service logs`/`proxy logs` flag conventions:
  `-n/--lines` (default 20), `-g/--grep` (substring on action or message),
  `--status success|failed`, `--json` (one JSON object per line, with a
  `host` field added), `-f/--follow` (`tail -f` on the raw file, requires
  exactly one host, always raw JSON regardless of `--json` since reformatting
  a streamed byte pipe isn't worth the complexity). `--stats` reads each
  selected host's full live project log and reports overall, per-action, and
  per-server entry counts, success rates, and average durations. `--since`
  limits stats to a compact relative window such as `30m`, `12h`, or `7d`;
  `--json` returns one structured aggregate object. Entries written before
  `duration_ms` existed still count toward totals and success rates but are
  excluded from duration averages, whose timed-entry coverage is reported.
  There is no local cache. `-S`/`--services` is
  rejected the same way `jiji lock` rejects it: the trail is host-scoped, not
  service-scoped. Every state-changing command writes to it: `jiji deploy`,
  `service restart`/`rollback`/`remove`/`prune`/`scale` (one entry per server
  via the shared `audit::record_endpoints_by_server` helper, summarizing
  every endpoint touched on that server during the run; `rollback`'s entries
  also carry the target `--version`), `jiji lock acquire`/`release`, `jiji
  network backup`/`restore`/`compact`/`import`, and `jiji
  server setup`/`teardown`. `server setup` writes its entry from the final
  (kamal-proxy) phase, since engine install and network setup each already
  bail the whole command on any per-host failure before reaching it -- a host
  that gets that far already succeeded at every earlier step. `server
  teardown` writes its entry *after* removing the project's staging
  directory (`.jiji/{project}`, which the audit log itself lives under, and
  which is removed early since it also holds plaintext secrets) -- this
  deliberately recreates that directory containing nothing but the one
  `server_teardown` entry, so a forensic record that the project was torn
  down survives the teardown that produced it, without resurrecting any
  secret-bearing scratch data.

## Known Gaps

- External `SecretsAdapter` (e.g. a Doppler-style adapter) — schema-only:
  `Config.secrets` parses but no runtime code path reads it, so configuring
  `secrets:` today changes nothing and produces no warning. `.env` files and
  host-env fallback are implemented; no adapter implementations exist. See
  `plans/followup.md` for the concrete integration plan.
- `network_mode: "host"` / `"none"` are documented in `crates/jiji-config/
  src/jiji.yml` but not implemented by any runtime code path —
  `container_runtime::build_dynamic_run` never reads `network_mode` for
  either value, so a service configured with them still gets normal bridge
  networking, silently. Only `"bridge"` (default) and `"service:<name>"`
  (see "Container Namespace Sharing" above) actually change behavior today.
- kamal-proxy's own `--health-check-cmd` execs using the raw `--target`
  address as a container reference (confirmed live: `podman exec
  <address> ...` fails with "no such container", since jiji always gives
  kamal-proxy raw IP targets, never a container name). This is a bug in the
  separate `ghcr.io/acidtib/kamal-proxy:jiji` Go binary/fork, not fixable in
  this Rust repo — a `cmd`-based `healthcheck:` works for jiji's own
  pre-activation gate (which execs by the candidate's real container name)
  but never for kamal-proxy's own ongoing route health check. See
  `plans/followup.md`.

## Testing

No mock-object framework: SSH-dependent integration tests spin up a real
in-process SSH server using russh's own `server` module (see
`crates/jiji-cli/tests/server_setup_test.rs`, `deploy_test.rs`,
`server_teardown_test.rs` for the exact `TestServer`/`spawn_test_server`/
`CannedResponse` pattern — a `HashMap<String, CannedResponse>` of exact
command strings to canned exit code/stdout/stderr, defaulting unmatched
commands to success). Tests run the compiled `jiji` binary as a real
subprocess (`Command::new(env!("CARGO_BIN_EXE_jiji"))`) and assert on exit
status, stdout/stderr, and (for ordering-sensitive tests) a shared
`received: Arc<Mutex<Vec<String>>>` command log.

Pure-function logic (command rendering, naming rules, config-derived
candidates) gets plain `#[cfg(test)] mod tests` unit tests co-located in the
same file — no SSH involved.

Local (non-SSH) engine invocations are tested against a fake `docker`/
`podman` executable placed first on `PATH` that logs argv and stdin to files
for assertions (see `registry_teardown_test.rs`, `registry_auth_test.rs`) —
use this instead of the SSH-mock pattern when the command never leaves the
local machine.

`jiji-agent`'s own tests (`replication.rs`, `catalog_replication.rs`,
`wireguard.rs`, `local_reconcile.rs`, etc.) don't go through SSH or the CLI at
all: they spin up two or more real `AgentStore`s over a real loopback
`TcpListener`/`TcpStream` pair and exercise `sync_once`/`serve` directly,
asserting on the resulting durable state -- e.g. that a write on one store
reaches the other without CLI fan-out, that a wrong-project or
mismatched-protocol/schema exchange is rejected before anything is applied,
or that an offline peer converges once it returns. Use this pattern (not the
SSH-mock harness) for anything that tests replication, membership, or catalog
behavior in isolation from the CLI.

**Important:** this mock-SSH suite is necessary but not sufficient. Several
real bugs only ever surfaced against real hosts: Docker's `ps --format`
`.Labels` being a flat string rather than a map (and the Podman inverse:
`.Label "key"` doesn't exist on Podman's `ps` reporter, it needs
`index .Labels "key"`); kamal-proxy always emitting ANSI color codes even
non-interactively; Podman refusing to push/pull the local loopback registry
over plain HTTP unless `--tls-verify=false` is passed, where Docker trusts
`localhost` registries by default; and `jiji server exec
--interactive` hanging forever after the remote shell exited, because
`tokio::io::stdin().read()` was polled fresh inside a `tokio::select!` loop
each iteration — cancelling that future does not cancel the underlying
blocking read, and the Tokio runtime waits for its blocking pool to drain on
shutdown, so a still-blocked stdin read (input that will never arrive once
the remote side is done) hung process exit indefinitely. Only reproduced
against a real allocated PTY (a captured-pipe test subprocess has no
controlling terminal to hang on); fixed by reading stdin on a dedicated
`std::thread` forwarding through an `mpsc` channel instead. Live-test CLI-
command-rendering work against a real Docker (and ideally Podman) host, and
interactive/PTY work against a real terminal, before considering it done —
`cargo test` passing is not sufficient evidence for anything that shells out
to `docker`/`podman`/`kamal-proxy`/`systemctl`/`nft`, or that drives a local
TTY.

Three more real-hosts-only bugs from Phase 9's live validation, none of which
any mock test could have caught: `jiji_network::proxy_script::
render_nftables`'s ingress DNAT rules matched on destination port alone,
with no destination-address restriction -- one chain (`output`) silently
hijacked the host's own outbound HTTPS/apt traffic, found only after a real
reboot broke package installs; the other (`prerouting`) hijacked genuine
cross-host container-to-container mesh traffic on the same ports, found via
`tcpdump` on a live cluster (fixed by scoping both to `ip daddr
{public_host}` and removing `output` entirely). Separately, kamal-proxy's
own network namespace keeps an ARP cache independent of the host's, so a
dynamically-leased address reused by a fresh container can sit `STALE` in
kamal-proxy's cache (old MAC) for tens of seconds before re-resolving,
surfacing as "no route to host" on a concurrent deploy -- only reproducible
against two real, concurrently-deploying hosts, never against the mock SSH
harness.

## Writing style

- Do not use emojis anywhere: code, comments, commit messages, or chat replies.
- Do not use em-dashes. Use commas, colons, parentheses, or separate sentences.
- Avoid filler "LLM-tell" phrasing. Write plainly and directly.

## Code comments

- Comment to explain why something is done or to flag a non-obvious constraint.
- Do not write summary comments that just restate what the next line does.
- Skip section-header and narration comments. Let the code speak for itself.

## Code Guidelines

- Conform to codebase conventions: follow existing patterns, helpers, naming,
  and formatting; if you must diverge, state why
- Optimize for correctness and clarity; avoid risky shortcuts or speculative
  changes
- Keep type safety: changes should pass `mise build`/`cargo build`; prefer
  proper types over stringly-typed workarounds
- DRY: search for prior art before adding new helpers or logic; reuse or
  extract shared helpers instead of duplicating
- Tight error handling: no broad catches or silent defaults; propagate errors
  with `anyhow`/`thiserror` and surface them explicitly
- Actionable error messages: every user-facing error must tell the user what
  to DO, not just what went wrong
- Efficient edits: read enough context before changing a file; batch logical
  edits together instead of many tiny patches

## Git

- Never add a co-author trailer to commits (no "Co-Authored-By" line).
- Keep commit messages short and factual.
- Run `/resume-work` at the start of a session to pick up context from
  previous sessions
- Never use `git commit --no-verify` — if hooks fail, fix every issue before
  committing
- Never use destructive commands (`git reset --hard`, `git checkout --`)
  unless explicitly approved
- Never force push to main
- No revert commits for unpushed work: use `git reset HEAD~1` instead of
  `git revert`
- Do not amend a commit unless explicitly requested
- Treat all `cargo clippy` warnings as bugs — run `mise lint` and fix before
  committing
- OSV scanner findings are blockers: run `mise scan` and use
  `/fix-osv-finding` to remediate; never dismiss without analyzing
  reachability

## Workflow

- Default expectation: deliver working code, not just a plan
- When working within the existing design system, preserve established
  patterns and visual language
- Commit at logical stopping points using `/commit`
- Pause after completing a task and wait for input before continuing

## External References

- Docs site: `~/Code/jiji-website` (Next.js/Nextra site under `app/docs/`) is
  the single source of user-facing documentation (architecture, deployment
  guide, testing guide, configuration/network/registry/logs/commands
  reference, troubleshooting, CI/CD) -- read the mdx files in that repo
  directly.
- POC archive: `~/Code/jiji-POC` (a prior Deno/TypeScript proof-of-concept
  with a different, superseded design — Corrosion, per-container rename
  deploys, a separate `jiji-dns` binary — kept only for feature-parity
  checks against this codebase's current behavior).

When in doubt about current behavior, read the Rust source in `crates/`
rather than relying on memory.
