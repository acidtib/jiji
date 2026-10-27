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
mise test-docker        # real Docker-in-Docker integration suite (needs Docker); see "Testing"
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
a stable service VIP: a **logical replica** (stable `replica_id`, one per
`(server, local_index)` pair -- `servers:` lists literal deploy targets and
`scale: N` puts N replicas on each one, survives redeploy) points at exactly
one **deployment** (the actual container, replaceable). Each deploy leases a
fresh address, commits the candidate to
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

### Host Networking (`network_mode: host`)

A service can share the host's own network namespace via `network_mode:
"host"` instead of getting a project-bridge address. It still goes through
the full candidate/active/draining/tombstone catalog lifecycle like a
bridge-networked service (`deploy_host_mode` in `deploy_transaction.rs`),
it just never calls the agent's `AllocateAddress` RPC and uses the server's
`management_address` (its WireGuard mesh address) everywhere a bridge
deployment would use its leased address: catalog commit, `jiji.lease`
label. `NetworkedContainerRun::host` renders `--network host` plus
`--dns*` flags (still needed for `.jiji` resolution; unlike
`service:<name>` sharing, a host-mode container inherits no one else's
resolver config) and never `-p`: `ports:` accepts at most one bare
container-side port as routing/uniqueness metadata only, since `-p`
combined with `--network host` is a discarded, warning-spamming no-op on
current Docker/Podman and a hard error on some older Podman releases.
`proxy:` and `scale > 1` stay rejected by the same generic non-bridge
checks `service:<name>` sharing already triggers. A `service:<name>`
dependent cannot target a `host`-mode upstream either: validation rejects
it, since joining a host-networked container's namespace would silently
hand the dependent host networking too, without it ever declaring
`network_mode: host` itself. `network_mode: none` is rejected outright by
validation: every service needs a reachable address for DNS and health
checks, and the one thing `none` is good for (an isolated one-off) is
already served by `crons:`.

A container's `jiji.lease` label carries `management_address` for a
`host`-mode container, or the upstream's address for a `service:<name>`
dependent -- kept purely for observability, since neither ever holds a real
per-deployment bridge lease. `jiji.lease-owned` (`true` only for a genuine
dynamic bridge lease, `false` for both of the above) tells agent-side
discovery (`recover_labeled_leases` in `jiji-agent/src/discovery.rs`) which
is which: without it, discovery would durably record that observability-only
address as a real claim on every agent restart, a row nothing ever cleans up
for a `host`-only project (cleanup only runs from the bridge
`AllocateAddress` path). A container with no `jiji.lease-owned` label at all
(deployed by a `jiji-cli` build older than this label) still gets the
previous unconditional recovery attempt, so an in-place agent upgrade never
regresses recovery for an already-running bridge deployment ahead of its
next redeploy.

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
an ownership transfer, left behind because its `crons:` entry was
renamed/deleted, or left behind on a host the service's own `servers:` no
longer lists -- requires connecting to every eligible agent and asking what
it actually has installed, which is why `cron_reconcile.rs` extends the
caller's SSH session pool beyond whatever `-H`/`-S` selected: to the
service's own `servers:` for installing, and, whenever the service has
`crons:` configured, further out to every server in the *project*
(`config.servers`) for the sweep, since a host dropped from `servers:` can
still have a stale spec. A service with no `crons:` configured never
widens beyond its own `servers:`, so it never touches a server outside
them. The sweep always runs (list installed specs, remove whatever isn't in
the current desired set), even when `service.crons` is now empty. The
owning agent's own scheduler (`jiji-agent/src/scheduler.rs`) claims and runs
jobs itself, in a fresh one-off container per run (`cron_exec.rs`), reusing
the durable `AddressAllocator`/`address_leases` machinery
(`cron_replica_id(service, cron_name)` naming) rather than any new leasing
path. `missed_runs: skip` collapses any pile-up of missed ticks by
comparing the schedule's natural next occurrence against "next after now"
-- no separate startup-recovery pass needed.

`jiji-agent`'s systemd unit needs `KillMode=process` (not the default
`control-group`): Podman here runs `cgroup_manager = cgroupfs` (see
"Container Engine Provisioning" below), so a container's conmon/crun
process stays in the unit's own cgroup, and any agent restart would
otherwise `SIGKILL` every container the agent manages, `jiji-proxy`
included. Agent-spawned cron containers are also relative-path- and
subnet-sensitive since `jiji-agent` spawns them directly, never over SSH.

Full detail, including two regressions found by code review (a stale-spec
sweep gap and a drift-check false positive) and the confirmed-live gotchas
behind them: `docs/architecture-notes.md#scheduled-jobs`. A third
regression, the `servers:`-shrink sweep gap above, was found by real-host
testing instead and is documented in the "Docker-in-Docker integration
suite" section below; not yet folded into `docs/architecture-notes.md`.

### Image Retention Reconciliation (`image_retention_reconcile.rs`)

Rhymes with cron reconciliation above (reuses `resolve_sessions`/
`close_newly_opened` from `cron_reconcile.rs`): a build-configured service's
`retain: N` spec is pushed to every host in its `servers:` list after a
successful deploy, and swept from any host whose service dropped `build:`,
changed `servers:`, or was renamed/deleted.

Full detail, including two regressions found by code review (an
`-H`-scoped sweep gap, and a `build:`-to-`image:` cutover that deleted a
still-protected retained image): `docs/architecture-notes.md#image-retention-reconciliation`.

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

- Logical replica: `placement::replica_id_for(project, service, server,
  local_index)`, stable across redeploys (and stable under an unrelated
  server being added/removed, or `scale` changing).
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
`.github/workflows/release-please.yml`, `.github/workflows/release.yml`).
There is no single repo-wide version, no manual bump command; never
hand-edit a crate's `version` or the manifest outside a release-please PR.

The flow is split so nothing is tagged before CI passes on it.
`release-please.yml` (every push to `main`) only opens/updates the release
PR (action input `skip-github-release: true`, a per-run switch unrelated to
the per-package config option in the gotcha below) and keeps a summary
comment on it (`.github/scripts/release-pr-summary.py`); the PR body itself
must stay as release-please wrote it, since release-please parses it again
to create the tags. `release.yml` runs on CI completing (`workflow_run`)
and only acts when CI succeeded on a push of a release commit (message
starting `chore: release`): it self-heals, runs release-please in tag-only
mode (`skip-github-pull-request: true`), then calls the build/publish
workflow for each tag created, in the same run. Nothing triggers on tag
pushes. While a merged release PR is still untagged (its CI running),
release-please refuses to open or update a release PR; it catches up on the
next push. If a release commit's CI fails, re-run it (a successful re-run
triggers `release.yml` again) or dispatch `release.yml` by hand. Both
workflows authenticate with a GitHub App (`RELEASE_APP_CLIENT_ID`
variable, `RELEASE_APP_PRIVATE_KEY` secret), never a personal token.

All 8 crates get an independent tag (`vX.Y.Z` for `jiji`, `{package}-vX.Y.Z`
for the other 7) and a GitHub Release, but only `jiji`, `jiji-agent`, and
`jiji-proxy` have a build/publish workflow (`jiji-release.yml`,
`jiji-agent-release.yml`, `jiji-proxy-release.yml`, called by `release.yml`
or dispatched by hand to rebuild an existing tag).
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

**Gotcha (confirmed live, don't reintroduce):**
`.github/scripts/self-heal-stuck-release.sh` (runs in `release.yml` right
before release-please-action's tag step, and reports the tags it creates as
a step output so the same run builds them) recovers a merged release PR release-please's own
`getBranchComponent`/`getComponent` bug (upstream #2214) drops the
tag/release for -- but that bug only occurs when the PR released exactly
one package, and that package has `include-component-in-tag: false` (only
`jiji-cli`, here). The script must gate on that exact precondition before
touching anything: a release PR is *always* still labeled `autorelease:
pending` for the brief window before `release-please-action`'s own step,
later in the same job, processes it -- that's normal, not evidence of
being stuck. Confirmed live: an earlier version of this script queried and
self-healed *any* merged-and-pending PR with no such gate, so it also
fired on a PR that released 5 packages together (a normal case, unaffected
by the bug), pre-creating all 5 tags/releases; `release-please-action`'s
own attempt right after then failed outright with `Release already
exists` on the first tag it reached, failing the whole job even though
every tag/release had, by then, already been correctly created.

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
- `crates/jiji-cli/src/placement.rs`: `replica_id_for`/`assignments_for`,
  deterministic placement of `scale` instances onto each of a service's
  literal `servers:` targets.
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
- `jiji update [--check] [--release VERSION]`: detects and installs a newer
  `jiji` binary release, config-free like `init`/`version`. Resolves "latest"
  from the GitHub release API (`--release` skips that call entirely and
  always proceeds, even to an older release -- the rollback path); downloads
  the matching platform artifact and its `.sha256` sidecar and rejects a
  mismatch; installs atomically (temp file on the same filesystem, then
  rename) preserving the existing binary's permissions; `--check` reports
  versions without changing anything. Never touches remote servers,
  `jiji-agent`, or `jiji-proxy` -- prints `jiji server upgrade -e
  <environment>` as the next step for that, once per environment. Shares
  the release repo, artifact-naming grammar, and checksum algorithm with
  `bin/install.sh` (which now also verifies `.sha256` after download), not
  literal source code. `self_update.rs` has the pure/network logic;
  `commands/update.rs` is the thin orchestration.
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
- `jiji server upgrade [-y/--yes]`: reads each selected host's live
  `jiji-agent` (its `Health` RPC) and `jiji-proxy` (`jiji-proxy version`)
  and compares both against what this CLI was built against. Per host,
  per component: outdated -> upgraded (agent binary replaced, proxy
  recreated); current -> config/unit/membership refreshed without a
  binary/container change; ahead of required -> never touched, reported
  as such (never downgrades); unavailable -> skipped, reported, command
  exits non-zero. Shares `server setup`'s enrollment machinery
  (`gather_membership_view`/`enroll_agent_targets` in
  `commands/server/setup.rs`, both `pub(crate)` for this reason) rather
  than duplicating it. Runs `jiji network diagnostics` at the end;
  `-S`/`--services` is rejected (host-level component, not per-service).
  Replaces the old manual `server setup` + `proxy restart` +
  `network diagnostics` sequence `jiji update` used to print.
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
  non-blockingly checks other peers' catalogs after success. Explicitly
  pulls each selected prebuilt `image:` service (`pull: always`, the
  default) on its selected hosts before any transaction, so a moving tag
  updates; a failed pull aborts before any mutation, even with a cached
  copy (`pull: missing` opts out). `restart`/`rollback`/`scale` never
  re-pull. See `docs/architecture-notes.md#health-gated-deployment-transaction`.
- `jiji build` and `jiji deploy --build`: local Docker/Podman builds,
  multi-arch publishing, remote registries, a loopback-only local registry
  exposed via temporary reverse SSH tunnels. `builder.remote` runs the build
  on a dedicated remote host instead of the local engine.
  `services.<name>.build.secrets` mounts ALL_CAPS names resolved from
  `.env`/host-env into the build via `--secret id=<name>,src=<path>`
  instead of `--build-arg` (never from `environment.clear`), staged to a
  mode-0600 temp file locally or piped over SSH stdin to the builder
  remotely; `DOCKER_BUILDKIT=1` is set automatically for classic
  single-arch `docker build` when secrets are configured.
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
  Server host references, build arguments, `build.secrets`,
  `environment.secrets`, `builder.registry.password`, and proxy SSL
  certificate references resolve from `.env`/host-env. Build arguments
  first read the merged service `environment.clear` map; `build.secrets`
  deliberately never does. Other secret-shaped fields (SSH key passphrases
  and `${VAR}` interpolation) are visibility-only.
- `jiji proxy restart` / `jiji proxy logs`: unconditionally re-pull and
  recreate the shared per-host jiji-proxy container, or read its logs
  (`--follow` requires exactly one host).
- `jiji service logs/restart/rollback/remove/prune/scale`: singular
  `service`. `restart`/`rollback` use the same configured replacement
  strategy and `deploy_endpoint` primitive as `jiji deploy`. `remove` locks
  only the endpoint's actually-owned replicas, tombstones the catalog record;
  `--volumes` also removes named volumes. `prune` enforces `service.retain`
  (build-configured services only, keeps first N image tags, removes the
  rest unless still referenced), deliberately left unlocked. `scale N -S
  <service>` writes a distributed desired-scale override (N instances on
  each of the service's already-listed `servers:`) under one `ServiceScale`
  lock keyed by service name; `N` is a positional argument, prompted for
  interactively if omitted.
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
  scale, prune, server setup/teardown/upgrade, network setup/compact/restore,
  registry login/logout, proxy restart, manual lock changes, `server exec`,
  `service cron run`, and `jiji build` (the `builder.remote` path only -- a
  local build opens no SSH session, so there is no host to write an entry
  through). `registry teardown`, local registry login/logout, and `network
  recover` are local-only operations with the same no-host reasoning, and are
  unaudited by design, not by gap. Writes are best-effort and never mask or
  block the command's own outcome. `-n/--lines`, `-g/--grep`, `--status`,
  `--json`, `-f/--follow`, `--stats` (with `--since`, e.g. `30m`/`12h`/`7d`).
  `-S`/`--services` is rejected: the trail is host-scoped.

## Known Gaps

- External `SecretsAdapter` (e.g. a Doppler-style adapter), schema-only:
  `Config.secrets` parses but no runtime code path reads it, so configuring
  `secrets:` today changes nothing and produces no warning. `.env` files and
  host-env fallback are implemented; no adapter implementations exist.

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

A command that talks to a plain HTTP service instead of SSH (`jiji update`
against GitHub's release API) is tested the same "roll a real local server"
way, but with a minimal in-process `TcpListener` HTTP/1.1 stand-in instead
of an SSH server (see `update_test.rs`).

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

### Docker-in-Docker integration suite (`mise test-docker`)

`crates/jiji-cli/tests/docker_*_test.rs` run the real `jiji` binary over real
SSH against `vm1` (and, for the mesh test, `vm2` too), **privileged**
containers defined in `test/docker/compose.yml` that boot their own real
systemd (not the mock-SSH suite's in-process russh server). This is the
layer the mock-SSH suite explicitly cannot cover: `jiji server setup` really
installs Podman via its own static-binary path onto the host, the
`jiji-agent-*.service` unit really starts under systemd, a WireGuard
interface really comes up against the host kernel's `wireguard` module,
`jiji deploy` really routes live HTTP traffic through jiji-proxy's real
nftables DNAT table to a real container, and `jiji deploy --build` really
builds and pushes through jiji's own locally managed registry container and
a real reverse SSH tunnel. `docker_mesh_deploy_test.rs` goes further: two
real hosts, a real WireGuard peer handshake between them, real cross-host
catalog/DNS replication (`vm1`'s own `.jiji`-zone resolver ends up knowing
both replicas' addresses after a deploy), and real cross-host L3
reachability -- `vm1` directly curling `vm2`'s replica address over the
WireGuard tunnel. All thirteen test binaries are pinned to nextest's
`docker-integration` test group (`max-threads = 1` in `.config/nextest.toml`)
since two concurrent `jiji server setup` runs against the same underlying OS
risk a real `apt`/`dpkg` lock race, not just resource contention. Modeled on
how [basecamp/kamal](https://github.com/basecamp/kamal)'s own CI runs
privileged nested-Docker containers as fake remote hosts; real nested-KVM VMs
were considered and rejected because `/dev/kvm` on standard GitHub-hosted
runners is undocumented and unreliable.

`vm1`/`vm2`'s image (`test/docker/vm/`) is deliberately minimal:
openssh-server, systemd, `iproute2`, `iputils-ping` (both needed only because
a bare `ubuntu:24.04` image ships without them, unlike a real cloud droplet
image -- `ip link`/`wg` need the former, `jiji server setup`'s own WireGuard
peer verification shells out to `ping` between mesh hosts and fails outright
without the latter, confirmed live). It does **not** pre-install Podman,
wireguard-tools, nftables, or dnsutils -- those come from the real `jiji
server setup` run under test, exercising the actual install path rather than
a stand-in for it. `test/docker/sshkeys/` generates one shared SSH keypair
into a host bind mount (`test/docker/.shared/`, gitignored) both hosts and
the Rust test harness (`crates/jiji-cli/tests/docker_support/mod.rs`) read.
For the two-host mesh test, `vm1` and `vm2` address each other over a
dedicated `mesh` compose network with static IPs (`172.30.0.11`/`.12`):
WireGuard needs both directions to dial out, which vm1's usual
`127.0.0.1:2201`-published-port address can't do (only the host machine can
reach that, not `vm2`) -- a Linux Docker bridge is directly routable from
the host itself, so no separate "deployer" container is needed the way
Kamal's design requires one.

Podman's storage root (`/var/lib/containers`) is a named Docker volume, not
part of `vm1`'s own container filesystem: jiji's production `storage.conf`
uses native overlay with no `mount_program`, and nesting a native overlay
mount directly on `vm1`'s own overlay diff is the same class of failure
Kamal hit with nested `dockerd` (they needed `fuse-overlayfs`). The volume
gives Podman a non-overlay backing filesystem instead, so jiji's real
production storage config works completely unmodified -- if that mitigation
ever turns out to be insufficient in practice, the fallback is a CI-only
`storage.conf` override written by `vm1`'s boot script *before* `jiji server
setup` runs, never a change to jiji's own install code.

Gated behind `JIJI_DOCKER_TESTS=1` (each test calls
`docker_support::skip_unless_enabled()` and returns early otherwise), so
plain `mise test`/`cargo nextest run --workspace` stays fast and
dependency-free; `mise test-docker` brings the compose stack up, runs the
suite, and tears it down. A release is only tagged and built after CI,
including this suite, passes on the release commit (see "Version
Management & Releases" above), so it is never cut on the strength of
mocked SSH output alone.

### CI (`.github/workflows/ci.yml`)

- `changes` runs `.github/scripts/ci-changes.sh` first and decides what
  the rest needs. On a PR, a change confined to crates runs `test` with a
  nextest `rdeps(<crate>)` filter. A change to `Cargo.toml`/`Cargo.lock`,
  `.config/`, `lib/`, `test/`, or CI itself runs everything, and so does
  every push. Docs-only changes skip Rust work entirely. The script runs
  locally too (`EVENT_NAME=pull_request BASE_REF=main
  .github/scripts/ci-changes.sh`).
- `build` compiles once into a nextest archive plus `jiji-agent`. `test`
  and the three `docker-in-docker tests (N/3)` shards (`--partition
  slice:N/3`, one vm1/vm2 per runner; `count:` splits within each binary,
  and every docker binary holds a single test, so it would put all of
  them on shard 1) extract it into the workspace
  (`--extract-to "$PWD"`), so compile-time `env!("CARGO_BIN_EXE_jiji")`
  paths still resolve. The archive does not carry `jiji-agent`, so the
  docker shards get it as a separate artifact file next to `jiji`.
- `clippy` (`-D warnings`), `clippy-macos` (`jiji-cli`, a shipped
  target), `fmt`, `deps` (`cargo update --locked`, `cargo shear`,
  `osv-scanner` against `osv-scanner.toml`), and `workflows` (actionlint,
  shellcheck, zizmor with `.github/zizmor.yml`) run alongside.
- `ci-pass` is the single required check: it fails unless every job
  succeeded or was skipped. Every job sets `timeout-minutes`, and every
  third-party action is pinned to a commit SHA (`@<sha> # vX.Y.Z`).
  `dtolnay/rust-toolchain` pinned by SHA needs an explicit `toolchain:`
  input, since it normally reads the toolchain from the ref name.
- To keep runner usage down: release-please PRs skip everything; on `main`
  the docker shards run only for release commits (every other commit
  already ran them as a PR); a newer push cancels an older run, except a
  release commit's, which gets its own concurrency group; closing a PR
  cancels its in-flight run; and only `main` saves the cargo cache, since
  PRs can only restore caches from the default branch.

**Scope, deliberately narrow for now:** single-host `server setup` -> real
state assertions (`podman info`, `systemctl is-active`, `wg show`) ->
`server teardown` (`docker_server_lifecycle_test.rs`); single-host `server
setup` -> `deploy` of one `nginx:alpine`-image, HTTP-proxied service -> a
real HTTP request through jiji-proxy -> `service remove` -> `server teardown`
(`docker_deploy_test.rs`); and single-host `server setup` -> `deploy --build`
of a service built from a real local Dockerfile (no `registry:` block, so
jiji manages its own local registry container end to end) -> a real HTTP
request proving the freshly built image is what's actually running ->
`service remove` -> `registry teardown` -> `server teardown`
(`docker_build_deploy_test.rs`); and a redeploy whose candidate never
becomes healthy (a healthcheck path nginx always 404s on, short
`deploy_timeout`) -> asserts the previously active container's ID is
completely unchanged and the failed candidate leaves no stray container
behind (`docker_rolling_deploy_test.rs`, the core non-negotiable invariant
in "Health-Gated Deployment Strategy" above); and a two-host `server setup`
-> real WireGuard peer handshake -> `deploy` of a two-replica service ->
real cross-host catalog/DNS replication -> real cross-host L3 reachability
over the tunnel -> `service remove` -> `server teardown`
(`docker_mesh_deploy_test.rs`, cross-host reachability); a two-replica,
two-host proxied service -> 40 requests through jiji-proxy on one host,
asserting more than one distinct response body comes back (each replica
serves its own container hostname) -> `service remove` -> `server teardown`
(`docker_load_balancer_test.rs`, proves jiji-proxy's actual backend
*selection* alternates across hosts, not just that both replicas are
independently reachable -- what the mesh test above deliberately leaves
unverified). And single-host `server setup` -> deploy
of a `network_mode: host` service alongside a `network_mode: service:<name>`
dependent -> real `podman inspect` state proving each actually landed in the
namespace it was supposed to (`.HostConfig.NetworkMode`, and for the
dependent, an exact `.NetworkSettings.SandboxKey` match with its upstream --
the real shared-netns file, not just similarly-worded config) -> `server
teardown` (`docker_network_mode_test.rs`). That test also confirmed
something the mock-SSH suite couldn't have caught: `jiji-proxy` binds host
ports 80/443 unconditionally once `server setup` has run, regardless of
whether any service in the project actually configures `proxy:` --
`network_mode: host` services in this suite must pick a different port.

Single-host `server setup` -> `deploy` of a service with one `crons:` entry
-> `jiji service cron list` confirms real installation on the owning
replica's agent -> `jiji service cron run` triggers an immediate run ->
`jiji service cron logs` (polled, not `run --follow`: an `echo` command
finishes before `--follow` can attach in practice, confirmed live) confirms
the run's real output -> `server teardown` (`docker_cron_test.rs`). And a
`build:`-configured, `retain: 2` service redeployed 4 times with a
genuinely distinct Dockerfile each time (an *unchanged* Dockerfile builds
to the exact same image ID regardless of `--version`, leaving nothing
prunable at all, confirmed live) -> asserts the real local image cache ends
up at exactly 2 tags, the 2 most recent -> `server teardown`
(`docker_image_retention_test.rs`; confirmed live that `jiji-agent`'s own
continuous reconciliation, not just the explicit `jiji service prune`
this test also calls, was already enforcing the count before that call
ever ran).

Two-host `server setup` -> `deploy` of a service (both hosts listed) with a
`crons:` entry -> `jiji service cron status` confirms `owner=vm1` (lowest
ordinal, alphabetically first) -> redeploy with vm1 dropped from *just the
service's own* `servers:` (vm1 stays in the project's top-level `servers:`,
still fully reachable) -> `cron status` confirms ownership really moved to
`owner=vm2`, and each host's own `jiji-agent` control socket
(`RequestBody::CronSpecList` -- ground truth independent of the CLI's
"current owner" lookup) confirms vm1's now-stale spec was actually swept
-> `server teardown` (`docker_cron_ownership_transfer_test.rs`). That
redeploy originally surfaced a real gap this test caught: `cron_reconcile.
rs::reconcile_service_crons` built its sweep session set from
`&service.servers`, the service's own *current* list, so once a host
dropped out of it the sweep loop structurally could never reach it again,
regardless of anything still installed there -- distinct from the three
already-fixed cron-sweep bugs `docs/architecture-notes.md#scheduled-jobs`
documents (a renamed/deleted `crons:` entry, a drift-check hash mismatch,
an old `-H`-scoping bug), none of which covered a host leaving the
service's own `servers:` entirely. Fixed by widening the sweep to every
server in the project (`config.servers`) whenever the service has `crons:`
configured, gated on that so a service with no cron jobs never touches a
server outside its own `servers:` (`restart_without_hosts_filter_never_
contacts_an_unrelated_server` in `service_restart_test.rs` guards exactly
this).

Single-host `server setup` -> `deploy` of a service with `scale: 2` ->
asserts two distinct containers exist and jiji-proxy alternates between
both same-host replicas across 40 requests -> `jiji service scale 1 -S
web -y` -> asserts the retired replica's container is actually removed,
not just stopped -> `service remove` -> `server teardown`
(`docker_scale_test.rs`; `docker_load_balancer_test.rs` above already
proves proxy alternation across *hosts*, this proves it across replicas on
the *same* host, which is what `scale:` actually means). And a service
with a direct host-port binding (`ports: ["8082:80"]`, no `proxy:`) ->
first deploy succeeds -> a second deploy with the same config (no
`stop_first`) fails outright, because the candidate tries to bind the same
host port the still-active previous container already holds, and the
previous container is left completely untouched (the same rolling
invariant `docker_rolling_deploy_test.rs` proves for a failed health check,
here for a failed container start instead) -> the same redeploy with
`stop_first: true` succeeds, since the previous container is stopped first
to free the port before the candidate ever tries to bind it -> `service
remove` -> `server teardown` (`docker_stop_first_test.rs`).

Single-host `server setup` -> `deploy --build --version v1` of a service
built from a Dockerfile baking in a "v1" marker -> `deploy --build
--version v2` of the same service built from a genuinely different
Dockerfile (an *unchanged* Dockerfile builds to the same image ID
regardless of `--version`, confirmed live) -> `jiji service rollback -S
web --version v1` -> asserts the v1 content is back and the container was
actually replaced, not reused -> `jiji service restart -S web` -> asserts
the currently-active (v1) image is reused unchanged but the container is
still replaced -> `service remove` -> `registry teardown` -> `server
teardown` (`docker_service_restart_rollback_test.rs`). Rollback needs no
live reverse tunnel to pull v1 back: it's already resident in vm1's own
Podman cache from the first build (`ensure_image` only pulls a missing
image), so this doesn't need to reopen the tunnel `deploy --build` used.
Every other docker test exercises the shared `deploy_endpoint` transaction
only through plain `jiji deploy` reruns; this is the only one that invokes
`jiji service restart`/`jiji service rollback` themselves.

Not yet covered: Ubuntu 26.04's AppArmor local-include path (its guard only
fires when the packaged `wg`/`wg-quick` profiles exist, which they don't on
this suite's Ubuntu 24.04 base image); `service.stop_first` combined with
`scale > 1` (rejected by validation, so nothing to exercise live);
`builder.remote`; `jiji network backup/restore/recover`; jiji-proxy
TLS/ACME and raw TCP (`listen_port`) relay mode; and `jiji server setup
--rotate-key`/`--import`. Don't assume broader real-host coverage than
this until that follow-up work lands.

## Writing style

- Do not use emojis anywhere: code, comments, commit messages, or chat replies.
- Do not use em-dashes. Use commas, colons, parentheses, or separate sentences.
- Avoid filler "LLM-tell" phrasing. Write plainly and directly.

## Comments

Write comments the way a human working under deadline pressure would: short,
and only when the line above genuinely needs it. Default to no comment.

**One clause, not a paragraph.** If a comment needs three lines to justify
itself, that's a sign to cut it down to the one fact that matters, not to
keep the reasoning trail.

Before:
```ts
// A fresh connection means this window's panel is open again, overriding
// any pending exclusion from a previous disconnect (e.g. the
// SIDE_PANEL_CLOSE blip can disconnect and reconnect a panel in another
// window). Also re-syncs once the id is known, in case the very first
// sync (right after onConnect, before this message arrives) ran while
// getContexts() hadn't yet caught up to the new panel.
closingWindowIds.delete(windowId)
```
After:
```ts
// A fresh connection means this window's panel is open again, overriding any pending exclusion.
closingWindowIds.delete(windowId)
```

**Don't narrate what you just did or why you chose this fix.** A comment
describes the code as it stands, not the story of the bug that led to it.
Never write "fixed X because Y used to do Z"; that belongs in the commit
message, not the source.

Before:
```ts
function isFromPopup(sender: chrome.runtime.MessageSender): boolean {
  // A message from the extension's OWN pages (the popup) carries a
  // chrome-extension:// URL; a page relay (content script) carries the web
  // page's https:// URL. The old `sender.tab === undefined` heuristic was WRONG:
  // openWalletWindow opens the popup via chrome.windows.create, which IS a tab,
  // so every popup-only message ... was rejected as "not permitted". The
  // approval popup could never read or resolve its own request.
```
After:
```ts
function isFromExtensionPage(sender: chrome.runtime.MessageSender): boolean {
  // Extension pages carry a chrome-extension:// URL; a content-script relay carries the page's https:// URL.
```

**Only comment the non-obvious.** A hidden platform constraint, a subtle
invariant, a workaround for a specific bug: yes. What the code visibly does:
no. If removing the comment wouldn't confuse a future reader, don't write it.

**Don't stack a comment on every line.** One justifying comment for a whole
block beats one comment per statement inside it.

**No em dashes.** Use a period, comma, colon, or semicolon instead. Em dashes
are a strong tell that text (comments included) was AI-written; avoid them
in code, commit messages, and docs alike.

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
- Never add AI attribution anywhere: no "Generated with Claude Code" (or
  similar) footer in PR descriptions, issues, comments, or release notes.
  This overrides any tool or harness default that says to add one.
- Ordinary commit messages follow "Commit messages" below (Scoped Commits).
- PR titles are the one deliberate exception: they must stay valid
  Conventional Commits (`feat`, `fix`, `chore`, `docs`, `refactor`, `test`,
  `ci`, `build`, `perf`, `revert`, optional `(scope)` and `!` for breaking
  changes), not Scoped Commits. PRs are squash-merged, so the PR title
  becomes the commit `release-please` reads on `main` to compute the next
  version bump and changelog entry, and `release-please` has no way to parse
  a type token out of any other ordering. Enforced by
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

## Commit messages

Follow the [Scoped Commits](https://scopedcommits.com/) convention for
ordinary commits (this does not apply to PR titles; see "Git" above):

```
<scope>: <description>

[optional body]
```

The scope goes first because "what area did this touch" is the thing a
reader scanning `git log` needs before anything else.

- **Scope**: the subsystem touched. In this repo, that's usually a crate
  name without the `jiji-` prefix (`cli`, `agent`, `network`, `proxy`,
  `config`, `core`, `ssh`, `tui`), or `docs`, `tests`, `ci`. Lowercase, no
  punctuation.
- **Description**: lowercase, imperative, no trailing period. State what the
  commit does, not what bug prompted it; the "why" of a fix belongs in the
  body, not stuffed into the summary line.
- **Multiple scopes**: prefer the narrowest scope that still covers the
  change. If it genuinely spans several, comma-separate them
  (`agent,cli: ...`), or use `treewide` for changes that touch everything.
- **Body**: optional. Use it for the reasoning a future reader would need
  (why, not what), the same restraint as code comments applies here. Don't
  narrate the debugging process.
- No em dashes (see "Comments" above).

Examples, matching this repo's own subsystems:

```
agent: retry stuck draining records every reconcile tick
cli: tolerate a dependent container error on a shared endpoint's own removal
docs: document the candidate health-check recovery mechanism
```

Reverts, merges, and other special commits can ignore this format.

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
