# AGENTS.md

Current invariants, ownership boundaries, transaction ordering, and source
pointers for the architecture summarized below live in
`docs/architecture-notes.md`. Read the relevant section before changing that
subsystem.

## Workspace Structure

Cargo workspace, eight crates in `crates/`:

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
│                 # server: durable local store, replicated catalog, incremental
│                 # WireGuard repair, distributed DNS, container reconciliation,
│                 # scheduled per-service cron execution.
├── jiji-proxy/   # The `jiji-proxy` binary: Pingora-based ingress proxy, host-global and
│                 # shared across projects (see "jiji-proxy" under "Private Networking"
│                 # below).
└── jiji-cli/     # The `jiji` binary: commands, orchestration, everything else
```

`jiji-cli` produces two binaries: `jiji` (the single `[[bin]]` entry in
`crates/jiji-cli/Cargo.toml`, path `src/main.rs`) and `jiji_dev`
(auto-discovered from `src/bin/jiji_dev.rs`, a debug binary for iterating
locally without overwriting an installed `jiji`).

## Quick Start Commands

```bash
# Build / run (via mise, or plain cargo)
mise build              # cargo build
mise run                # cargo run --
mise test               # cargo nextest run --workspace (falls back to cargo test if nextest isn't installed)
mise test-cli           # same, scoped to -p jiji-cli
mise test-agent         # same, scoped to -p jiji-agent
mise test-verbose       # mise test with --no-capture / -- --nocapture
mise fmt                # cargo fmt
mise lint               # cargo clippy --all-targets --all-features && cargo fmt --check
mise check              # cargo check
mise scan               # osv-scanner -r . (vulnerability scan, see fix-osv-finding skill)

# Install locally
mise install             # cargo build --release --bin jiji --bin jiji-agent -> ~/.local/bin/{jiji,jiji-agent}
mise install-dev         # cargo build --bin jiji_dev --bin jiji-agent -> ~/.local/bin/{jiji_dev,jiji-agent}

# Single test file / crate
cargo test -p jiji-cli --test deploy_test
cargo test -p jiji-network
```

## High-Level Architecture

### Command Flow (CLI)

`clap` (derive) for command routing (`crates/jiji-cli/src/cli.rs` defines
`Cli`, `Commands`, `ServerCommands`, `NetworkCommands`). No shared
`setupCommandContext()`-style helper; each command's `run()` in
`crates/jiji-cli/src/commands/` repeats the same sequence inline:

```
load_config() -> validate_config() -> build NetworkPlan (if needed) ->
select hosts (NetworkPlan::select_hosts) -> connect via SshPool -> execute -> close sessions
```

`NetworkPlan::select_hosts` matches `-H`/`--hosts` filters against both a
server's config key name and its `host:` address (`ServerPlan.public_host`).
`-H app1` matches a server named `app1` regardless of its `host:` value,
and also matches any server whose `host:` address is literally `app1`.

`crates/jiji-cli/src/lib.rs::run()` is the shared entrypoint for both `jiji`
and `jiji_dev`.

### CLI Output Conventions

All command output goes through `jiji-tui`'s `Ui` helpers, never ad-hoc
`println!`: `Ui::section` for headings; `Ui::progress`/`result_ok`/
`result_warn`/`result_error` as stable `OK`/`SKIP`/`FAIL` rows, not
free-form logging; `Ui::success_elapsed` on completion; `Ui::warn`/`error`
for warnings/errors. ANSI styling auto-disables when output isn't a real
TTY. Never gate this manually per command. Default output describes intent
and outcome, not underlying shell commands (those stay verbose-only).

### Configuration System

`jiji-config` (`crates/jiji-config/src/schema.rs`) defines the full config
schema as plain `serde`-deserializable structs: no lazy-loaded getters, no
base-class hierarchy. `load_config()` searches upward from cwd for
`.jiji/deploy.yml` or `jiji.{environment}.yml`. `validate_config()` returns a
`ValidationResult` with explicit errors, not throw-on-first-error.

`crates/jiji-config/src/jiji.yml` is the authoritative configuration
reference (same file `jiji init` writes as a template).

### Distributed Control Plane

Every server runs a project-scoped `jiji-agent` (`crates/jiji-agent/`) that
owns a durable local store, incrementally repairs its own WireGuard peers,
serves project DNS from its local catalog, and reconciles its own
containers/leases/proxy routes on restart. Membership has no key material
and no peer-to-peer relay: `jiji-cli` computes it locally from `jiji.yml`
and pushes it directly over SSH to every reachable host (`jiji-agent
membership-import`). A host's trust boundary is "this file was installed by
root." The service catalog and desired-state placement are genuinely
node-originated at runtime, so they keep continuous **direct-only**
peer-to-peer anti-entropy (never relayed through a third node): a receiver
authenticates an inbound record by resolving the TCP connection's source
address against its local membership view, since WireGuard's own peer
authentication makes that address unspoofable within the mesh.

Non-negotiable invariants: a service deployment never rewrites WireGuard; a
targeted deploy normally connects to and locks only its affected host and
logical replica; temporary absence of a host or peer never means permanent
deletion, only an explicit tombstone does; DNS only ever publishes an
`active`+`healthy` catalog record. Capacity per project: 32 nodes (full
WireGuard mesh), 500 services, 2,000 logical replicas. Validation rejects
cardinality above these outright.

Full mechanism, protocol/schema-version rejection, and record-provenance
authentication detail: `docs/architecture-notes.md#distributed-control-plane`.

### Health-Gated Deployment Strategy

Jiji uses **dynamically leased deployment addresses**, not fixed A/B slots or
a stable service VIP: a **logical replica** (stable `replica_id`, survives
redeploy) points at exactly one **deployment** (the actual container,
replaceable). Each deploy leases a fresh address, commits the candidate to
the catalog *before* starting it (durable even if the start fails),
health-checks it directly at its own address (never through the proxy), and
only touches jiji-proxy once the catalog already marks it `Active`/`Healthy`.
If any step through proxy activation fails, the previous container and route
are left untouched and keep serving traffic; only the candidate is torn
down. `service.stop_first: true` is a distinct transaction that stops the
previous container first, for services that can't tolerate two running
instances.

Rolling services use a zero-downtime transaction. Direct host-port bindings
cannot coexist during replacement, and `service.stop_first: true` deliberately
stops the previous container first.

Full transaction: `docs/architecture-notes.md#health-gated-deployment-transaction`.

### Private Networking (WireGuard mesh + agent-served DNS)

Per-project isolated, not a host-global singleton: WireGuard interface,
bridge network, and agent state are all derived purely from `config.project`
(`crates/jiji-network/src/naming.rs`), so two independent projects can share
one physical host without sharing project network or control-plane state.
`jiji-agent` owns WireGuard
peers/DNS/catalog continuously once bootstrapped; `jiji network setup`
covers one-time mesh bootstrap only, never re-run by a service deploy except
to reconcile a genuinely stale host.

**jiji-proxy** (`crates/jiji-proxy/`) is deliberately the **one shared,
multi-tenant** component: one container per host, multi-homed across every
project's bridge with active routes. HTTP routes are keyed by
`(host, path_prefix)`, while raw TCP routes are keyed by `listen_port`.
The proxy continuously re-resolves the **aggregate**
`{project}-{service}.jiji` DNS name and load-balances across whatever it
discovers mesh-wide. This is what gives it genuine cross-host load balancing.
A wildcard `hosts:` entry (`*.example.com`)
matches exactly one DNS label (`foo.example.com` matches; `deep.foo.
example.com` doesn't); an exact-host route always wins over a wildcard.

`proxy.listen_port` selects raw TCP relay mode. `port` remains the backend
container port. Raw TCP targets cannot use HTTP-only `path_prefix` or `ssl`,
ports 80 and 443 remain reserved for HTTP ingress, and every shared-host TCP
route needs a unique public port. `hosts` is optional metadata for TCP routes,
not a routing key. Validation catches conflicts within one project;
jiji-proxy rejects cross-project conflicts when applying the route. The main
implementation paths are `jiji-cli/src/proxy_routes.rs`,
`jiji-proxy/src/route_manager.rs`, and `jiji-proxy/src/tcp_relay.rs`.

Two gotchas worth knowing before touching this layer:
- Docker/Podman's IPAM has no knowledge of jiji's reserved addresses or
  current leases. Every jiji-managed container **must** pin `--ip`
  explicitly to an address `jiji-agent` itself leased, or the engine can and
  will hand out a reserved address (e.g. the DNS address) to an unrelated
  container (confirmed live).
- On Docker, jiji-proxy's own `--publish 80:8080 --publish 443:8443` silently
  drops its IPv4 binding (its bridges are NAT-disabled/routed, for routable
  backend addresses across the mesh). `proxy_ingress.rs` works around this
  with a host-global nftables DNAT table instead. Podman is unaffected.

Full mechanism (membership/catalog/DNS wiring, jiji-proxy ACME/TLS,
per-project identifier list, the ingress DNAT gotcha in full):
`docs/architecture-notes.md#project-networking` and
`docs/architecture-notes.md#shared-ingress-proxy`.

### Container Namespace Sharing (`network_mode: service:<name>`)

A service can share another ("upstream") service's container network
namespace instead of getting its own address, via `network_mode:
"service:<upstream-name>"` (the "VPN killswitch" pattern): naming the
upstream is itself the dependency declaration, no separate `depends_on`.
`validation.rs` rejects an undefined/self-referencing/chained upstream, or a
`servers:` list that isn't a subset of the upstream's. A dependent has no
address of its own to lease, no `healthcheck:`/`proxy:` of its own, and is
automatically cascaded into the deployment plan whenever its upstream is
selected (`add_cascaded_dependents`): deployed in two sequential waves
(upstream first, then dependents) since a dependent can't attach until the
upstream's new container exists.

Full detail: `docs/architecture-notes.md#container-namespace-sharing`.

### Scheduled Cron Execution (`crons:`)

Each service can define `crons:` (name -> `CronConfig`: `schedule`,
`timezone`, `command`, `timeout`, `overlap: forbid`, `missed_runs: skip`).
The CLI picks the lowest-ordinal Active/Healthy replica as the job's
*owner* (`cron_reconcile::select_cron_owner`) and pushes an idempotent
`CronSpecApply` to that replica's `jiji-agent` after every deploy/restart/
rollback (`reconcile_after_deploy`) or `scale` (calls `reconcile_service_crons`
directly instead, since scaling can move ownership without a redeploy),
whether or not the service currently has `crons:` configured; `service
remove` unconditionally removes every installed spec for the service. Cron
specs and run history are agent-local and deliberately never replicated
(unlike the catalog): finding a stale spec -- left on a former owner after
an ownership transfer, or left behind because its `crons:` entry was
renamed/deleted -- requires connecting to every eligible server's agent and
asking what it actually has installed, which is why `cron_reconcile.rs`
extends the caller's SSH session pool to the whole `servers:` list (not just
whatever `-H`/`-S` selected) and always sweeps every one of them (list
installed specs, remove whatever isn't in the current desired set), even
when `service.crons` is now empty. The owning agent's own scheduler
(`jiji-agent/src/scheduler.rs`) claims and runs jobs itself, in a fresh
one-off container per run (`cron_exec.rs`), reusing the durable
`AddressAllocator`/`address_leases` machinery (`cron_replica_id(service,
cron_name)` naming) rather than any new leasing path. `missed_runs: skip`
collapses any pile-up of missed ticks by comparing the schedule's natural
next occurrence against "next after now" -- no separate startup-recovery
pass needed.

Two bugs found by code review, not real-host testing: an earlier version of
the sweep above only removed specs whose names were still present in the
current `service.crons` map, so a renamed/deleted entry (or a service whose
`crons:` block went empty) left its old installation running forever,
undiscoverable by `service remove` either. Separately, `jiji service cron
list`'s drift check (`commands/service/cron/list.rs`) must recompute a
spec's expected hash using the exact same absolutized `env_file_path`/
`mount_args` the sweep above installs with (`remote_home_dir`/`absolutize`/
`absolutize_mount_args`, all `pub(crate)` from `cron_reconcile.rs` for this
reason) -- comparing against the unabsolutized form reports every installed
job as permanently `drifted`, regardless of whether anything actually
changed.

Two gotchas confirmed live, both because `jiji-agent` spawns cron
containers directly via `tokio::process::Command`, never over SSH:
- The `.jiji/{project}/...`-relative paths `stage_env_file`/
  `mounts::remote_mount_base` hand back only resolve against an *SSH
  login's* home directory. `jiji-agent`'s own cwd is `/` (no such login),
  so `cron_reconcile.rs` resolves the owner's home directory once
  (`remote_home_dir`) and sends `CronSpecApply` absolute paths instead.
- Lease cron container addresses from `mesh_config.local_runtime.
  container_subnet` (this host's actual bridge subnet), never
  `container_cidr` (the whole-mesh reserved range covering every project
  and server) -- the latter hands out addresses podman rejects outright as
  outside the bridge's subnet.

`jiji-agent`'s systemd unit also needs `KillMode=process` (not the default
`control-group`): Podman here runs `cgroup_manager = cgroupfs` (see
"Container Engine Provisioning" below), so a container's conmon/crun
process stays in the unit's own cgroup, and any agent restart -- an
upgrade, `Restart=on-failure` -- would otherwise `SIGKILL` every container
the agent manages, `jiji-proxy` included (confirmed live; `podman ps` kept
reporting the dead container "Up" since conmon never got to record an
orderly exit).

Full detail: `docs/architecture-notes.md#scheduled-jobs`.

### `jiji server teardown` (inverse of `server setup`)

Order matters: `jiji-agent` is removed **first**, unconditionally, before
anything it reconciles, otherwise its own continuous reconcile loop
silently recreates proxy routes/containers moments after teardown reports
them removed (confirmed live). Then: proxy routes -> containers -> volumes
(`--volumes`) -> images -> shared jiji-proxy container (only if unused) ->
this project's network layer (WireGuard interface *and* config, bridge,
compiled network state). `-S`/`--services` is rejected. Another project's
resources on the same host are surfaced as an informational notice, never a
blocker.

Full detail: `docs/architecture-notes.md#teardown-ordering`.

### SSH Connection Management

`jiji-ssh` is built on **russh** (pure-Rust async SSH, no subprocess).
`SshPool` (semaphore-based) provides `execute_concurrent`/`execute_batched`/
`execute_with_error_collection` for fan-out across many hosts.
`crates/jiji-cli/src/ssh_adapter.rs` adapts `jiji_config::{NamedServer, Ssh}`
into `jiji_ssh::ConnectOptions`.

`jiji server setup` reads target membership through the sessions that hold
the `HostRuntime` locks. It pushes membership through the agent-install
sessions. Do not add separate target connections for these steps. The extra
connections can trigger common SSH firewall rate limits such as `ufw limit
ssh`. If a setup connection is refused, wait 31 seconds before one retry. Do
not retry during the wait because each rejected connection can refresh the
firewall limit.

Gotcha: a remote command killed by a signal never sends
`ChannelMsg::ExitStatus` (SSH sends `exit-signal` instead). `Option<u32>`
exit codes must treat `None` the same as a nonzero exit, never as success.

Full detail: `docs/architecture-notes.md#ssh-connection-semantics`.

### Deployment Locking

`crates/jiji-cli/src/lock.rs`: a mkdir-atomic SSH lock primitive over
`LockScope`, a fixed, deadlock-free rank order:

```
ProjectMaintenance (0) < HostRuntime (1) < ServiceScale (2)
  < LogicalReplica (3) < HostGlobalProxy (4)
```

A command computes its full lock set up front, sorts by `(rank, host, path)`,
acquires concurrently within a rank, sequentially rank-to-rank: every
command's lock set is a subset of one fixed total order, so two commands can
never deadlock. `LogicalReplica` (keyed by `replica_id`) is what `deploy`/
`service restart`/`rollback`/`remove` actually lock, so an unrelated offline
host never blocks a targeted operation.

Full rationale: `docs/architecture-notes.md#deployment-locks`.

### Naming Conventions

Quick-recognition patterns (full derivation/rationale in
`docs/architecture-notes.md#naming-and-ownership`):

- Logical replica: `placement::replica_id(project, service, ordinal)`,
  stable across redeploys.
- Deployment: fresh random `deployment_id` per container start.
- Container: `{project}-{service}-{first 12 hex chars of deployment_id}`.
- DNS: `{project}-{service}.jiji` (aggregate), `{project}-{service}-
  {server}.jiji` (per-replica).
- Ownership labels: `jiji.managed=true jiji.project=<p> jiji.service=<s>
  jiji.server=<srv> jiji.resource=service`.
- Per-project identifiers (`jiji-network/src/naming.rs`, pure functions of
  `project:` alone): WireGuard interface `jiji{8 hex}`, bridge device
  `jijib{7 hex}`, engine network `jiji-{slug}`, systemd unit `jiji-agent-
  {slug}.service`, WireGuard port `51820..=55819`, catalog replication port
  `58000..60000` (membership has no port of its own: it's CLI-pushed, not
  replicated).

### Version Management & Releases (release-please)

All 8 crates have an explicit `[package].version` (never
`version.workspace = true`: the `cargo-workspace` release-please plugin
hard-fails on any workspace member using it, since it scans every
`Cargo.toml` under `[workspace].members` regardless of release-please's
own `packages` config). Tracked by
[release-please](https://github.com/googleapis/release-please)
(`release-please-config.json`, `.release-please-manifest.json`,
`.github/workflows/release-please.yml`). There is no single repo-wide
version, no manual bump command; never hand-edit a crate's `version` or
the manifest outside a release-please PR.

All 8 crates get an independent tag (`vX.Y.Z` for `jiji`, `{package}-vX.Y.Z`
for the other 7) and a GitHub Release, but only `jiji`, `jiji-agent`, and
`jiji-proxy` have a build/publish workflow attached to that tag
(`jiji-release.yml`, `jiji-agent-release.yml`, `jiji-proxy-release.yml`).
The other 5 are internal-only: their tag/release exists purely so
release-please has an anchor for that package (see the loop gotcha below),
not because anyone should consume them directly. `cargo-workspace`
patch-bumps every crate that depends on whatever just bumped, so a `fix:`
to `jiji-core` cascades through `jiji-network` into `jiji-cli`/`jiji-agent`,
no `Release-As:` footer needed. `jiji-proxy` has no internal deps, so
nothing cascades into it.

**Gotcha (confirmed live, don't reintroduce):** a package configured with
`"skip-github-release": true` gets no tag at all, not just no public
Release page. Without a tag, release-please has no anchor to tell it that
package was already released, so on every subsequent run it re-surfaces the
*original* triggering commit and bumps that package again, forever,
cascading an empty-changelog patch bump onto every real-tagged dependent
too (confirmed live: this looped through 3 auto-generated release PRs,
each re-citing the same commit, before being caught). Every package in
`release-please-config.json` must get a real tag, even the internal-only
ones; use the build/publish workflow's presence, not `skip-github-release`,
to distinguish "consumable" from "internal-only."

**Gotcha (confirmed live, don't reintroduce):** internal
`[workspace.dependencies]` entries must stay bare `{ path = "..." }`, no
`version` field. Adding one makes `cargo build`/`check` semver-check it
against the crate's real version, since crates bump independently
(`bump-minor-pre-major: true` makes routine pre-1.0 minor bumps normal),
and the next minor bump on any internal crate breaks the whole workspace
build. It also wouldn't help changelogs anyway: release-please's
`CargoToml` updater skips any dep using `dep.workspace = true` (all of
them here).

Since release-please can't generate a useful changelog note for that
reason, `.github/scripts/expand-dependency-changelog.sh <package-name>`
does it independently: diffs `.release-please-manifest.json` against `git
show HEAD~1:...` to find what bumped, filters to the releasing package's
actual deps (`cargo metadata --no-deps`), and appends each bumped crate's
own `CHANGELOG.md` section under "### Crate changes in this release",
the only place an internal crate's real change description becomes
public. **Security:** always pipe a fetched release body into this script
via `env:`, never interpolate `${{ }}` directly into a `run:` block. It's
free-text commit/PR content.

`jiji-cli`'s and `jiji-network`'s `build.rs` each read a sibling crate's
version at compile time (`jiji-agent` → `AGENT_BUILD_VERSION`, `jiji-proxy`
→ `PROXY_VERSION`/`image()`) via a shared `sibling_crate_version()` helper,
`include!`-ed from `lib/build-support/sibling_crate_version.rs`, needed
because a fresh `jiji server setup` can no longer assume the agent/proxy
build to install/pull is "the same version as me".

## Key Files

- `crates/jiji-config/src/jiji.yml`: authoritative configuration reference
  (all options), also the template `jiji init` writes.
- `crates/jiji-config/src/schema.rs`: the full config schema.
- `crates/jiji-network/src/planner.rs`: `NetworkPlanner`/`NetworkPlan`, the
  deterministic per-server addressing/topology computation mesh bootstrap
  needs; carries no service/deployment state.
- `crates/jiji-network/src/naming.rs`: every project-derived name; the
  single source of truth the per-project isolation design depends on.
- `crates/jiji-cli/src/placement.rs`: `replica_id`/`endpoint_replica_id`,
  deterministic initial placement across a service's eligible servers.
- `crates/jiji-cli/src/deploy_transaction.rs`: the dynamic per-endpoint
  deploy transaction: lease, candidate commit, start, health-check, proxy
  activation, active/draining/tombstoned catalog commits.
- `crates/jiji-cli/src/lock.rs`: the ranked, scope-aware CLI lock primitive.
- `crates/jiji-agent/src/store.rs`: the durable local `AgentStore` (SQLite),
  the single source of truth every replicated/local-only structure is
  persisted through.
- `crates/jiji-agent/src/membership.rs`, `catalog.rs`: the record types
  (`MembershipRecord`, `CatalogRecord`), their validation/CRDT-convergence
  rules, and `RecordProvenance` (how a catalog/desired-state record's
  ownership is authenticated without a signature); `catalog_replication.rs`
  is the direct-only, peer-to-peer anti-entropy exchange that carries
  catalog/desired-state between hosts (membership has no peer-to-peer
  exchange).
- `crates/jiji-agent/src/runtime.rs`: wires catalog replication, DNS, and
  WireGuard repair together into the agent's continuous run loop.
- `crates/jiji-agent/src/local_reconcile.rs`: autonomous repair of local
  runtime state (bridges, routes, DNS binding, proxy attachment, container
  discovery) from durable catalog records plus local observations.
- `crates/jiji-agent/src/dns.rs`: the hand-rolled `.jiji` zone resolver
  served from the local replicated catalog.
- `crates/jiji-agent/src/leases.rs`: `AddressAllocator`, the durable
  per-deployment address lease/quarantine/recovery logic.

### Container Engine Provisioning

`engine::ensure_engine` (`crates/jiji-cli/src/engine.rs`), shared by `jiji
server setup` and `builder.remote` preflight. Debian/Ubuntu get a pinned,
SHA256-verified static Podman binary (distro packages are too old for
current CDI specs); Fedora/RHEL get `dnf install podman`. An
already-installed Podman below the minimum is upgraded in place. Podman execs
always pass `--no-session` (avoids a new PAM/login session per health-check
poll).

Two gotchas confirmed live on real Ubuntu 24.04 droplets, both blocking
`jiji server setup` outright until fixed: the static storage.conf's
`mountopt` must never include `fsync=0` (not a valid kernel overlayfs
option -- EINVAL on every container mount); mesh bootstrap's
`install_prerequisites` (`commands/network/setup.rs`) must run `loginctl
enable-linger` for the SSH user, otherwise a rootful container started over
SSH gets silently killed once that SSH session's systemd scope is cleaned
up.

Ubuntu 26.04 adds AppArmor profiles for `wg` and `wg-quick`. They otherwise
deny Jiji's root-only private key and immutable WireGuard configuration. Mesh
bootstrap must add narrow rules through the profiles' local includes and
reload both profiles. Systems without these profiles must take the no-op path.

Full detail: `docs/architecture-notes.md#container-engine-provisioning`.

## Command Reference

- `jiji version`: prints the running binary's version and git SHA.
- `jiji init`: scaffolds `.jiji/deploy.yml`, including project-directory-derived
  `/24` management and `/16` container CIDRs.
- `jiji server setup [-y/--yes] [--rotate-key] [--import] [--import-dry-run]`:
  engine install, then `jiji-agent` install-and-start (agent brings up
  WireGuard/bridge/DNS/jiji-proxy itself), one `HostRuntime` lock per
  targeted host. Also reconciles membership on every run: compares each
  target's freshly observed WireGuard public key/endpoint against its last
  known record (`commands::network::membership::reconcile_record`):
  endpoint-only drift bumps `revision`; a changed key fences a new
  `owner_epoch`. Any server still `Active` in the gathered mesh view but no
  longer in `servers:` is tombstoned (`compute_decommissions`). Both are
  gated by confirmation unless `--yes`, and bail actionably (never hang)
  with no TTY and no `--yes`. `--rotate-key` forces a fresh keypair on the
  targeted hosts, fenced into a new `owner_epoch` by the same reconcile
  pass. The private key never leaves the host or flows through
  `jiji.yml`. `--import`, once each host's agent is up, prints a read-only
  assessment (legacy runtime, enrollment, catalog count,
  importable/migrated/orphaned; see `commands::network::assess::assess_host`)
  then one-way seeds any pre-existing container as historical (`Stopped`)
  catalog history (`commands::network::import::run_import`). Never
  `Active`, never a lease, never an already-live replica, no lock beyond
  the `HostRuntime` locks already held. `--import-dry-run` prints the plan
  without committing. No standalone assess, import, decommission,
  update-endpoint, rotate-key, or replace command exists: `server setup`
  absorbs all of it.
- `jiji network plan` / `jiji network setup`: print or transactionally apply
  the deterministic mesh-bootstrap plan (WireGuard peers, bridge, reserved
  addresses only, no service/deployment state). Setup also migrates an
  existing bridge when configured CIDRs change, with rollback on activation
  failure.
- `jiji network catalog` / `diagnostics [--json]`: read-only inspection of a
  host's locally replicated catalog, or its agent's self-healing/
  replication/quota/component diagnostics.
- `jiji network backup --output --passphrase-file` / `restore` / `recover`:
  export an encrypted backup (catalog/desired-state operations, address
  claims; never membership, WireGuard keys, or secrets: membership is
  re-derived from `jiji.yml` by `server setup`, not backed up), restore into
  surviving hosts in the same epoch, or recover into a new fenced epoch
  (`recover` requires `--yes`; the epoch is a plain fencing counter, `.jiji/
  recovery/`, unrelated to any key). After `recover`, run `jiji server
  setup` on replacement hosts, then redeploy desired services.
- `jiji network compact`: compacts each selected host's superseded
  replicated operation history.
- `jiji deploy`: health-gated deploy (see above): mounts, env/secrets,
  dynamic address leasing, health checks, jiji-proxy routing, `-H`/`-S`
  filtering, `stop_first`, optional builds, mesh reconciliation only when a
  targeted host is stale. Prints the plan and prompts for confirmation
  before touching anything; `-y` skips it; no TTY and no `-y` bails
  actionably rather than hanging. `--wait-for-peers <N>` optionally,
  non-blockingly checks other peers' catalogs after success.
- `jiji build` and `jiji deploy --build`: local Docker/Podman builds,
  multi-arch publishing, remote registries, a loopback-only local registry
  exposed via temporary reverse SSH tunnels. `builder.remote` runs the build
  on a dedicated remote host instead of the local engine.
- `jiji registry login` / `logout`: credentials on the local machine and/or
  `-H`-selected servers, password over stdin only.
- `jiji registry teardown`: removes the exact local `jiji-registry`
  container, with `--dry-run` and typed or `--yes` confirmation.
- `jiji server teardown`: full inverse of `server setup` (see above),
  including `--dry-run`, `--volumes`, typed project-name confirmation.
- `jiji server exec`: runs a command on `-H`-resolved servers (concurrently
  by default, `--sequential` for one at a time), or attaches an interactive
  shell/PTY to exactly one (requires `-H` to resolve to a single server).
  `-S`/`--services` is rejected.
- `jiji-ssh`: connect, auth (key files, inline key data, ssh-agent
  fallback), `ProxyCommand`/ProxyJump, streaming exec, PTY, `sftp_put`/`get`
  (standalone primitive, no CLI command uses it yet), pooled concurrent
  execution.
- `jiji secrets print`: non-fatal `.env`/host-env resolution status
  (`[SET]`/`[MISSING]`, `--show-values`, `-S` filter).
  Server host references, build arguments, `environment.secrets`,
  `builder.registry.password`, and proxy SSL certificate references resolve
  from `.env`/host-env. Build arguments first read the merged service
  `environment.clear` map. Other secret-shaped fields (SSH key passphrases and
  `${VAR}` interpolation) are visibility-only.
- `jiji proxy restart` / `jiji proxy logs`: unconditionally re-pull and
  recreate the shared per-host jiji-proxy container, or read its logs
  (`--follow` requires exactly one host).
- `jiji service logs/restart/rollback/remove/prune/scale`: singular
  `service`. `restart`/`rollback` use the same configured replacement
  strategy and `deploy_endpoint` primitive as `jiji deploy`. `remove` locks
  only the endpoint's actually-owned replicas, tombstones the catalog record;
  `--volumes` also removes named volumes. `prune` enforces `service.retain`
  (build-configured services only, keeps first N image tags, removes the
  rest unless still referenced), deliberately left unlocked. `scale -S
  <service> --replicas N` writes a distributed desired-scale override under
  one `ServiceScale` lock keyed by service name.
- `jiji service cron list/status/run/logs`: scheduled per-service commands
  (see "Scheduled Cron Execution" above). `list`/`status` read installation
  state and durable run history from the owning replica's agent; `run
  <cron> -S <service>` requests an immediate out-of-schedule run (rejected
  as an actionable conflict if one is already active); `logs <cron> -S
  <service> [--run <id>] [-f]` streams a run's container output.
- `jiji lock acquire/release/status/show`: scope-aware locking (see
  "Deployment Locking" above); `release --replica <id>` / `--service <name>`
  / `--scope host-runtime|proxy` targets a specific stuck lock.
- `jiji audit`: a per-project, per-server, append-only JSONL trail at
  `.jiji/{project}/audit.log`. Current writers are deploy, service lifecycle,
  scale, prune, server setup/teardown, and manual lock changes. Writes are
  best-effort and never mask or block the command's own outcome.
  `-n/--lines`, `-g/--grep`, `--status`, `--json`, `-f/--follow`, `--stats`
  (with `--since`, e.g. `30m`/`12h`/`7d`). `-S`/`--services` is rejected:
  the trail is host-scoped.

## Known Gaps

- External `SecretsAdapter` (e.g. a Doppler-style adapter), schema-only:
  `Config.secrets` parses but no runtime code path reads it, so configuring
  `secrets:` today changes nothing and produces no warning. `.env` files and
  host-env fallback are implemented; no adapter implementations exist. See
  `docs/todo.md` for the concrete integration plan.
- `network_mode: "host"` / `"none"` are documented in `crates/jiji-config/
  src/jiji.yml` but not implemented by any runtime code path:
  `container_runtime::build_dynamic_run` never reads `network_mode` for
  either value, so a service configured with them still gets normal bridge
  networking, silently. Only `"bridge"` (default) and `"service:<name>"`
  (see "Container Namespace Sharing" above) actually change behavior today.
- Audit coverage is incomplete. Network, registry, and proxy mutations do not
  write audit entries. See `docs/todo.md` for the coverage plan.

## Testing

No mock-object framework: SSH-dependent integration tests spin up a real
in-process SSH server using russh's own `server` module (see
`crates/jiji-cli/tests/server_setup_test.rs`, `deploy_test.rs`,
`server_teardown_test.rs` for the exact `TestServer`/`spawn_test_server`/
`CannedResponse` pattern, a `HashMap<String, CannedResponse>` of exact
command strings to canned exit code/stdout/stderr, defaulting unmatched
commands to success). Tests run the compiled `jiji` binary as a real
subprocess and assert on exit status, stdout/stderr, and (for
ordering-sensitive tests) a shared `received: Arc<Mutex<Vec<String>>>`
command log.

Pure-function logic (command rendering, naming rules, config-derived
candidates) gets plain `#[cfg(test)] mod tests` unit tests co-located in the
same file, no SSH involved.

Local (non-SSH) engine invocations are tested against a fake `docker`/
`podman` executable placed first on `PATH` that logs argv and stdin to files
for assertions (see `registry_teardown_test.rs`, `registry_auth_test.rs`).
Use this instead of the SSH-mock pattern when the command never leaves the
local machine.

`jiji-agent`'s own tests (`catalog_replication.rs`, `membership.rs`,
`wireguard.rs`, `local_reconcile.rs`, etc.) don't go through SSH or the CLI
at all: catalog/desired-state replication tests spin up two or more real
`AgentStore`s over a real loopback `TcpListener`/`TcpStream` pair (binding
the outbound side to a distinct loopback address, e.g. `127.0.0.2`, so
`RecordProvenance::Peer`'s source-address check has something real to
authenticate against) and exercise `sync_once`/`serve` directly. Membership
tests don't need any of that: they exercise `MembershipView::apply` directly
against plain records.

**Important:** this mock-SSH suite is necessary but not sufficient. Live-test
CLI command rendering against a real Docker host and, when possible, Podman.
Test interactive and PTY work in a real terminal. Networking, nftables,
systemd ownership, and teardown changes require real-host validation. See
`docs/architecture-notes.md#testing-boundaries` for the constraints mock tests
cannot cover.

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
- PR titles must be valid Conventional Commits (`feat`, `fix`, `chore`,
  `docs`, `refactor`, `test`, `ci`, `build`, `perf`, `revert`, optional
  `(scope)` and `!` for breaking changes). PRs are squash-merged, so the PR
  title becomes the commit `release-please` reads on `main` to compute the
  next version bump and changelog entry. Enforced by
  `.github/workflows/pr-title-lint.yml`. Which crate(s) a commit directly
  bumps is decided by its *changed paths* (`crates/<name>`), not by the PR
  title's type/scope: a PR touching two crate directories bumps both
  directly, and the `cargo-workspace` plugin cascades from there to every
  crate that depends on what changed (see "Version Management & Releases"
  above).
- Run the resume-work skill at the start of a session to pick up context from
  previous sessions
- Never use `git commit --no-verify`: if hooks fail, fix every issue before
  committing
- Never use destructive commands (`git reset --hard`, `git checkout --`)
  unless explicitly approved
- Never force push to main
- No revert commits for unpushed work: use `git reset HEAD~1` instead of
  `git revert`
- Do not amend a commit unless explicitly requested
- Treat all `cargo clippy` warnings as bugs: run `mise lint` and fix before
  committing
- OSV scanner findings are blockers: run `mise scan` and use
  fix-osv-finding skill to remediate; never dismiss without analyzing
  reachability

## Workflow

- Default expectation: deliver working code, not just a plan
- When working within the existing design system, preserve established
  patterns and visual language
- Commit at logical stopping points using `/commit`
- Pause after completing a task and wait for input before continuing

## External References

- `docs/architecture-notes.md` (this repo): current invariants, ownership
  boundaries, transaction ordering, failure semantics, and source pointers
  for the architecture summarized above.
- Docs site: `~/Code/jiji-website` (Next.js/Nextra site under `app/docs/`) is
  the single source of user-facing documentation (architecture, deployment
  guide, testing guide, configuration/network/registry/logs/commands
  reference, troubleshooting, CI/CD) -- read the mdx files in that repo
  directly.
- POC archive: `~/Code/jiji-POC` (a prior Deno/TypeScript proof-of-concept
  with a different, superseded design: Corrosion, per-container rename
  deploys, a separate `jiji-dns` binary, kept only for feature-parity
  checks against this codebase's current behavior).

When in doubt about current behavior, read the Rust source in `crates/`
rather than relying on memory.
