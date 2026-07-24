# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with
code in this repository.

> Jiji was originally a Deno/TypeScript monorepo (`packages/cli`, `packages/dns`,
> `packages/daemon`) built against a proof-of-concept design. That codebase has
> been fully replaced: this repository is now a pure Rust Cargo workspace. There
> is no `packages/` directory anymore. If you find references to Deno, Cliffy,
> `packages/`, Corrosion, or a separate `jiji-dns`/`jiji-daemon` binary anywhere
> outside historical docs, treat them as stale.

## Workspace Structure

This is a Cargo workspace with six crates in `crates/`:

```
crates/
├── jiji-core/     # Shared primitives: pattern matching, error types, default CIDRs
├── jiji-tui/      # Terminal UI helpers (Ui::say/section/confirm/confirm_typed/spinner)
├── jiji-config/   # Config schema, YAML loading, validation (jiji.yml reference lives here)
├── jiji-network/  # Deterministic private-network planning (NetworkPlanner, NetworkPlan)
├── jiji-ssh/      # SSH abstraction over russh (SshSession, SshPool)
└── jiji-cli/      # The `jiji` binary: commands, orchestration, everything else
```

`jiji-cli` produces two binaries (see its `[[bin]]` entries and
`src/bin/jiji_dev.rs`): `jiji` (the real one) and `jiji_dev` (a separate debug
binary for iterating locally without overwriting an installed `jiji`).

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

`NetworkPlan::select_hosts` matches `-H`/`--hosts` filters against each
server's `host:` address (`ServerPlan.public_host`), never its config key
name — `-H app1` only matches a server literally named `app1` if its `host:`
value is `app1`; otherwise pass the actual address/pattern.

`crates/jiji-cli/src/lib.rs::run()` is the shared entrypoint for both the
`jiji` and `jiji_dev` binaries; it dispatches on `Commands`/`ServerCommands`/
`NetworkCommands` and prints a consistent error shape for every command.

### Configuration System

`jiji-config` (`crates/jiji-config/src/schema.rs`) defines the full config
schema as plain `serde`-deserializable structs (`Config`, `NamedServer`, `Ssh`,
`Service`, `ProxyConfig`, `MountConfig`, etc.) — no lazy-loaded getters, no
`BaseConfiguration` base class. `load_config()` searches upward from cwd for
`.jiji/deploy.yml` or `jiji.{environment}.yml`. `validate_config()` returns a
`ValidationResult` with explicit errors, not a throw-on-first-error model.

`crates/jiji-config/src/jiji.yml` is the authoritative configuration reference
(same file `jiji init` writes as a template).

### Zero-Downtime Deployment Strategy

The Rust rewrite replaced the old TypeScript POC's rename-based model
(`web` -> `web_old_{timestamp}`) with **fixed dual-address (A/B) backend
slots**: `jiji-network`'s `NetworkPlanner` deterministically assigns each
service endpoint two backend addresses (`BackendSlot::A`/`B`) and one stable
VIP address, up front, from config alone. There is no rename step.

Flow (`crates/jiji-cli/src/deploy_transaction.rs`, orchestrated by
`commands/deploy.rs`):

1. Compare the installed network generation across every configured server.
   If any host is stale, transactionally reconcile the full cluster before
   changing images, proxy routes, or service containers.
2. `network_guard::verify_generation` — recheck the selected endpoint's host
   immediately before its service transaction as a final race-condition guard.
3. `service_network::prepare_cutover` — read the currently active slot from
   `/etc/jiji/network/{slug}/service-nat-current/active-slots`; the candidate
   always goes on the *other* (inactive) slot.
4. Stage mounts (`mounts.rs`) and environment (`env_resolution.rs`, via a
   remote `--env-file`, never inline `-e KEY=VALUE`, so secrets never appear
   in a logged command).
5. Create and start the candidate container
   (`container_runtime::build_run` + `container_ops::create_and_start`) at
   its own fixed backend address — the VIP still points at the old backend.
6. Health-check the candidate directly (`health_check.rs`), never through the
   VIP.
7. `service_network::commit_after_health_check` re-runs the same health
   command as the authoritative gate, then flips the VIP (nftables DNAT) to
   the new backend.
8. Activate/verify kamal-proxy routes (`proxy_routes.rs`); roll back the VIP
   and remove the candidate if that fails.
9. Stop and remove the previous slot's container.

If health checks fail, the previous container is never touched and keeps
serving traffic through the still-unflipped VIP.

### Private Networking (WireGuard + compiled DNS, no Corrosion)

The POC's Corrosion (CRDT gossip database) is gone. The current design is a
**compiled, static mesh**: `jiji-network::NetworkPlanner` computes the entire
topology (WireGuard peers, per-server container subnets, VIPs, DNS records)
from config alone, with no runtime coordinator. `jiji network setup`
(`crates/jiji-cli/src/commands/network/setup.rs`) writes that compiled plan to
each host as an immutable, symlink-swapped "generation".

**Per-project isolated, not a host-global singleton.** Every name and path
below is derived purely from `config.project` (`crates/jiji-network/src/
naming.rs`) — two independent projects can run `jiji server setup` against
the same physical host and get two fully independent sets of the following,
with zero shared/persisted state between them (see "Naming Conventions"
above for the exact derivation and `docs/network-reference.md` for the
operator-facing explanation, including the residual hash-collision risk when
projects share default CIDR ranges):

- **WireGuard**: `wg-quick@{wireguard_interface}.service` (interface name
  `jiji{8 hex}`, one per project), config at
  `/etc/jiji/network/{slug}/generations/{gen}/wireguard.conf`, current
  pointed to by `/etc/wireguard/{wireguard_interface}.conf`. WireGuard port
  is also per-project (`51820..=55819`), not the fixed `51820`.
- **Bridge/engine network** (`commands/network/bridge.rs`): a
  `jiji-{slug}` docker/podman network per project (kernel device name
  `jijib{7 hex}`, distinct from the logical name because of Linux's 15-char
  interface limit); Podman additionally needs a `jiji-network-anchor-{slug}`
  keepalive container per project (Podman removes bridges with no attached
  containers).
- **Service VIP routing**: `jiji-service-nat-{slug}.service` applies an
  nftables table (`jiji_service_nat_{slug_with_underscores}`, one per
  project) that DNATs each service's stable VIP to whichever backend slot is
  currently active — `service_network.rs` is the only thing that ever
  mutates this state.
- **DNS**: `jiji-dns-{slug}.service` execs plain `dnsmasq` per project
  against a compiled `dns.conf` (`{project}-{service}.jiji` /
  `{project}-{service}-{server}.jiji` records) — there is no jiji-authored
  DNS binary.
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

Docker/Podman's own IPAM has no knowledge of jiji's reserved addresses
(`ServerPlan::dns_address`, `proxy_address`, backend slots): `dnsmasq` runs as
a host-level systemd service, not a container, so the engine can and will
hand out the DNS address to an ad-hoc container started on a jiji bridge
without an explicit `--ip` (confirmed live, pre-isolation, against the old
shared `jiji` bridge: `docker run --network jiji nginx:alpine` got assigned
the DNS address and silently broke resolution for that container — the same
risk applies to any project's `jiji-{slug}` bridge today). Every jiji-managed
container avoids this because `container_runtime`/`proxy.rs` always pin
`--ip` explicitly — any new code that runs a container on a jiji bridge
(debug tooling, health-check sidecars, etc.) must do the same.

### `jiji server teardown` (inverse of `server setup`)

`crates/jiji-cli/src/commands/server/teardown.rs` orchestrates the inverse of
everything above: proxy routes -> application containers -> volumes (with
`--volumes`) -> images -> the shared kamal-proxy container (only when no
project still has routes) -> VIP/NAT state -> disconnecting kamal-proxy from
this project's bridge (`proxy::disconnect_bridge_if_attached`, independent of
whether kamal-proxy is still running for other projects) -> this project's
own network layer (systemd units, WireGuard, nftables, bridge network,
compiled `/etc/jiji/network/{slug}` subtree only — never a sibling project's
subtree on a shared host) -> the per-project staging directory
(`env_resolution::project_staging_dir`, holds staged `.env` files with
resolved secrets and uploaded mount content). Ownership discovery is by
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

### Naming Conventions

- **Images**: explicit `image:` references, or versioned references produced by
  `jiji build` / `jiji deploy --build` from each service's `build:` config.
- **Containers**: `{project}-{service}-{a|b}` (permanent per backend slot, no
  rename).
- **Proxy targets**: `{project}-{service}-{app_port}` (per-server local route
  name in that server's own kamal-proxy — routes are not shared/synced across
  servers).
- **DNS records**: `{project}-{service}.jiji` (aggregate) and
  `{project}-{service}-{server}.jiji` (per-replica).
- **Ownership labels**: `jiji.managed=true jiji.project=<p> jiji.service=<s>
  jiji.server=<srv> jiji.resource=service` on every service container.
- **Per-project network identifiers** (`crates/jiji-network/src/naming.rs`,
  all pure functions of `project:` alone, computed the same way independent
  of whether any other project shares the host — see
  `docs/network-reference.md`'s "Multiple projects on one server"): WireGuard
  interface `jiji{8 hex}` (`wireguard_interface_name`), kernel bridge device
  `jijib{7 hex}` (`bridge_interface_name`, distinct from the logical bridge
  name below because Linux interface names are capped at 15 characters),
  Docker/Podman logical network `jiji-{slug}` (`bridge_network_name`),
  systemd units `jiji-dns-{slug}.service` / `jiji-service-nat-{slug}.service`
  / `jiji-network-restore-{slug}.service` (`systemd_unit_slug`), WireGuard
  port `51820..=55819` (`wireguard_port`), nftables table
  `jiji_service_nat_{slug_with_underscores}` (`service_nat_table_name`). All
  remote state lives under `/etc/jiji/network/{slug}/` instead of the old
  single shared `/etc/jiji/network/`.

## Key Files

- `crates/jiji-config/src/jiji.yml` — authoritative configuration reference
  (all options), also the template `jiji init` writes.
- `crates/jiji-config/src/schema.rs` — the full config schema.
- `crates/jiji-network/src/planner.rs` — `NetworkPlanner`/`NetworkPlan`, the
  deterministic address/topology computation.
- `crates/jiji-network/src/naming.rs` — every project-derived name (WireGuard
  interface/port, bridge interface/network name, systemd unit slug, nftables
  table name); the single source of truth the per-project isolation design
  depends on.
- `crates/jiji-network/src/service_runtime.rs` — `BackendSlot`,
  `ActiveSlotState`, `NetworkedContainerRun`, `ServiceNatArtifacts`.
- `crates/jiji-cli/src/service_network.rs` — VIP cutover primitives
  (`prepare_cutover`, `commit_after_health_check`, `rollback_cutover`,
  `reconcile_slots`, `deactivate_project`).

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

**Important:** this mock-SSH suite is necessary but not sufficient. Several
real bugs only ever surfaced against real hosts: Docker's `ps --format`
`.Labels` being a flat string rather than a map (and the Podman inverse:
`.Label "key"` doesn't exist on Podman's `ps` reporter, it needs
`index .Labels "key"`); kamal-proxy always emitting ANSI color codes even
non-interactively; Podman refusing to push/pull the local loopback registry
over plain HTTP unless `--tls-verify=false` is passed, where Docker trusts
`localhost` registries by default; and `jiji-network-restore.service`
stalling `server teardown` by about a minute under Podman because that
unit's `ExecStart` is what starts the `--restart unless-stopped` network
anchor container, so its `conmon` process lives in the unit's own cgroup and
`systemctl disable --now` waits out its stop timeout before SIGKILLing it —
fixed by removing the anchor before disabling the unit
(`network_teardown::remove_anchor_if_present`); and `jiji server exec
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

## Writing style

- Do not use emojis anywhere: code, comments, commit messages, or chat replies.
- Do not use em-dashes. Use commas, colons, parentheses, or separate sentences.
- Avoid filler "LLM-tell" phrasing. Write plainly and directly.

## Code comments

- Comment to explain why something is done or to flag a non-obvious constraint.
- Do not write summary comments that just restate what the next line does.
- Skip section-header and narration comments. Let the code speak for itself.

## Git

- Never add a co-author trailer to commits (no "Co-Authored-By" line).
- Keep commit messages short and factual.

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

## Git Discipline

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

---

# Rust Rewrite Status

The Deno/TypeScript codebase and its POC-era design (Corrosion, per-container
rename deploys, a separate `jiji-dns` binary) have been fully replaced. This
section tracks what's landed and what's still deferred.

## Implemented

- `jiji init` — scaffolds `.jiji/deploy.yml`.
- `jiji server setup` — container engine install (Docker/Podman version
  check, distro-aware install), full network setup, kamal-proxy provisioning.
- `jiji network plan` / `jiji network setup` — print or transactionally apply
  the deterministic network plan (idempotent, rollback on partial failure).
  The network layer is per-project isolated (own WireGuard interface/port,
  bridge, DNS resolver, compiled state tree per project — see "Private
  Networking" above), so multiple independent projects can share one server;
  kamal-proxy is the one intentionally shared, multi-homed component.
- `jiji deploy` — full zero-downtime deploy (see architecture section above):
  mounts, env/secrets, health checks, VIP cutover, kamal-proxy routing,
  `-H`/`-S` filtering, `stop_first`, optional image builds, and automatic
  network reconciliation.
- `jiji build` and `jiji deploy --build` — local Docker/Podman builds,
  multi-architecture publishing, remote registries, and a loopback-only local
  registry exposed to deployment hosts through temporary reverse SSH tunnels.
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
- `jiji server exec` — runs a command, or an interactive login shell, on
  exactly one `-H`-resolved server; `--interactive` attaches a PTY to a given
  command too. Local raw-mode terminal handling and resize forwarding
  (`SIGWINCH` -> `channel.window_change`) live in `commands/server/exec.rs`,
  not `jiji-ssh`. Automatically downgrades to non-interactive when stdin/
  stdout aren't a real TTY, with a warning. `-S`/`--services` is rejected.
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
  `ssh.key_passphrase`/`key_data`, `servers.*.host`, proxy SSL certs, build
  args, and `${VAR}` command interpolation are scanned and reported for
  visibility only (matching the POC's scan coverage) since the current SSH/
  proxy/build code paths use those fields as literal values with no
  env-var-reference resolution at all — `commands/secrets/print.rs` flags
  this gap explicitly in its own output rather than implying those fields
  resolve when they don't.
- `jiji proxy restart` / `jiji proxy logs` — unconditionally re-pull and
  recreate the shared per-host kamal-proxy container, or read its logs from
  selected hosts (`--follow` requires exactly one). Restart preserves the
  named configuration volume and bypasses the config-fingerprint no-op so a
  changed moving image tag is picked up. Log filters are shell-quoted.
- `jiji service logs/restart/rollback/remove/prune` — singular `service`, not
  `services` (the POC name). `logs` tails the currently active slot's
  container per selected endpoint (`--container-id` bypasses slot resolution
  entirely for an arbitrary container name; `--follow` requires exactly one
  target), sharing its command-rendering and streaming code with `proxy
  logs`. `restart` is a zero-downtime slot cycle built directly on the same
  `deploy_endpoint` primitive `jiji deploy` uses (health check, VIP cutover,
  old-slot cleanup), reusing `service.image` when set or otherwise
  discovering the currently running image by inspecting the active container
  (for build-only services with no static `image:`). `rollback` is the same
  `deploy_endpoint` slot cycle but for a caller-supplied `--version`
  (required) instead of whatever is currently running: a build-configured
  service resolves the target purely from `builder.registry` + project +
  service name (no rebuild, no per-endpoint SSH round trip, trusting the tag
  was already pushed by a prior `jiji build`/`jiji deploy --build`); a
  static-`image:` service gets `--version` applied the same way `jiji deploy
  --version` does, and is rejected the same way if the image already carries
  an explicit tag. `remove` stops/removes both A/B slot containers, removes
  any proxy routes, and deactivates the endpoint's VIP/NAT mapping;
  `--volumes` additionally removes the selected services' named volumes.
  `prune` implements the `service.retain` pruning that was previously
  deferred: lists each build-configured service's image tags per server
  (trusting the engine's own newest-first `images` ordering rather than
  parsing `CreatedAt`), keeps the first `retain` (or `--retain` override),
  and removes the rest unless still referenced by a container. Services with
  only a static `image:` (no `build:`) are never pruned.
- `jiji lock acquire/release/status/show` — a per-project deployment lock at
  `.jiji/{project}/deploy.lock` on each selected server (relative to the SSH
  user's home directory, same staging root `env_resolution::project_staging_dir`
  uses for uploaded env files), holding a message, acquirer, timestamp, and
  pid (`crates/jiji-cli/src/lock.rs`). `acquire` polls up to `--timeout`
  seconds (default 300) waiting for an existing lock to clear before giving
  up, or `--force` to override immediately. `jiji deploy` checks the lock
  before making any change and refuses to proceed if any selected server is
  already locked.
- `jiji audit` — a per-project, per-server, append-only JSONL trail at
  `.jiji/{project}/audit.log` (same staging root as the lock file and
  uploaded env files), each line one `{timestamp, action, status, actor,
  message}` object (`crates/jiji-cli/src/audit.rs`). Writes are best-effort
  (`audit::record`): a failed audit write is warned, never propagated, so
  audit logging can never mask or block the outcome of the command it's
  recording. Reads are `tail -n <lines>` per selected host, with malformed
  lines silently skipped (an audit line from a future incompatible format,
  or a rare concurrent-write interleaving, must never make the trail
  unreadable) rather than resolved from a POC-era `--filter`/`--since`/
  `--until`/`--raw`/`--aggregate` surface that was more aspirational in the
  old docs than actually implemented even there — `jiji audit` instead
  mirrors this codebase's own `service logs`/`proxy logs` flag conventions:
  `-n/--lines` (default 20), `-g/--grep` (substring on action or message),
  `--status success|failed`, `--json` (one JSON object per line, with a
  `host` field added), `-f/--follow` (`tail -f` on the raw file, requires
  exactly one host, always raw JSON regardless of `--json` since reformatting
  a streamed byte pipe isn't worth the complexity). `-S`/`--services` is
  rejected the same way `jiji lock` rejects it: the trail is host-scoped, not
  service-scoped. Every state-changing command writes to it: `jiji deploy`,
  `service restart`/`rollback`/`remove`/`prune` (one entry per server via the
  shared `audit::record_endpoints_by_server` helper, summarizing every
  endpoint touched on that server during the run; `rollback`'s entries also
  carry the target `--version`), `jiji lock acquire`/`release`, and `jiji
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

## Explicitly deferred (silent no-op today, not an error)

- External `SecretsAdapter` (e.g. a Doppler-style adapter) — schema-only:
  `Config.secrets` parses but no runtime code path reads it, so configuring
  `secrets:` today changes nothing and produces no warning. `.env` files and
  host-env fallback are implemented; no adapter implementations exist. See
  `docs/followup.md` for the concrete integration plan.

## Reference Archives

- Working copy: `~/Code/jiji`
- POC archive: `~/Code/jiji-POC` (same code, kept for reference — describes
  the superseded Deno/Corrosion/rename-based design; useful for feature
  parity checks, not for current architecture)

When in doubt about current behavior, read the Rust source in `crates/`
rather than relying on memory or the POC archive.
