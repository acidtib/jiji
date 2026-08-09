# Follow-Up Items

Tracked gaps and possible next steps that aren't part of any specific
in-progress work. Each item should stay concrete enough to act on directly;
update or remove an item once it's resolved rather than leaving it stale.

## kamal-proxy's `--health-check-cmd` execs by the raw target address, not a container name

**Status: confirmed live, not fixable in this repo.** `proxy_routes.rs`'s
`render_static_deploy_args` renders `--health-check-cmd`/
`--health-check-cmd-runtime` for kamal-proxy's own ongoing route health
check when `healthcheck.cmd` is configured. Confirmed live (while
diagnosing a `casa-lab` deploy failure): kamal-proxy execs this command
against the raw `--target` address as if it were a container name/ID
(`podman exec <address> <cmd>`), which fails outright --
`docker logs kamal-proxy` showed `"command failed (exit 125): Error: no
container with name or ID \"100.98.192.5\" found: no such container"`.
Since jiji's whole architecture deliberately always gives kamal-proxy raw
IP targets, never a `.jiji` hostname or container name (see "Private
Networking" in CLAUDE.md), `cmd`-based ongoing kamal-proxy health checks are
structurally broken for every jiji-managed route, not just one service.

This is a bug in the separate `ghcr.io/acidtib/kamal-proxy:jiji` Go
binary/fork (its own `--health-check-cmd` implementation), not something
fixable in this Rust repo. `cmd`-based healthchecks still work correctly for
jiji's *own* pre-activation gate (`health_check::plan_for_candidate`, which
execs by the candidate's real, known container name) -- only kamal-proxy's
own ongoing check is affected. Worth fixing in the kamal-proxy fork itself:
either resolve the target address back to a container name via engine
inspect before exec'ing, or accept a container name directly as an
alternative to an address for `--target` when a health-check `cmd` is
configured.

## Concurrent-deploy proxy activation: understand why the retry is needed at all

**Status: masked by a retry, root cause of the timing not fully understood.**
Phase 9 fixed the concurrent-multi-replica-deploy race (kamal-proxy getting
"no route to host" against a sibling replica's stale, already-superseded
address) by re-reading the catalog immediately
before proxy activation in `deploy_transaction.rs`, plus retrying once more
with a fresh read if that first activation attempt still fails. Live testing
after the fix showed 5/5 clean runs, but 3 of those 5 only succeeded via the
retry path (~109s, vs. ~18-20s for a clean first-attempt success) -- meaning
the *first* activation attempt is still regularly racing and losing against
the sibling replica's concurrent deploy well over half the time, and only the
added retry recovers it.

This is a real fix (it converts a hard failure into a reliable success), but
it's masking a timing question that was never actually answered: why is the
first attempt's catalog re-read (taken immediately before activation, after
this endpoint's own image pull/create/health-check has already completed)
still so often stale relative to the sibling's write? Two endpoints in a
`spread`-placed deploy should reach their own proxy-activation point at
roughly comparable times if the underlying create/health-check work is
similar, so a >50% first-attempt failure rate suggests something more
specific is skewing the timing consistently in one direction (in the live
testing session, the failure was always the same host, "nyc", never "sfo"),
not just generic jitter. Worth revisiting with fresh eyes:

- Instrument (temporarily) the actual wall-clock gap between when each
  endpoint's own candidate becomes healthy and when the sibling's catalog
  write actually lands, across several concurrent-deploy runs, to see if
  there's a structural asymmetry (e.g. one host's image pull/create/health
  check path is consistently slower, or SSH session setup/registry-tunnel
  overhead differs between the two hosts in a way that isn't obviously
  visible from the CLI's own timing).
- Check whether catalog replication itself (P2P sync interval, not just the
  agent's own local read) has a asymmetric propagation delay in one
  direction under concurrent write load from two hosts at once.
- Consider whether the retry's ~90s cost (a full health-check timeout
  elapsing before the retry even starts) should be shortened for this
  specific case -- if the sibling's write is expected to land within the
  same rough window as this endpoint's own transaction, waiting out the
  *entire* configured health-check timeout before retrying may be far
  longer than actually necessary.

## External `SecretsAdapter` (e.g. Doppler)

**Status: schema-only, not implemented.** `jiji.yml`'s `secrets:` block and
`jiji-config`'s `SecretsAdapter` struct (`crates/jiji-config/src/schema.rs`)
already exist and are documented in the config template
(`crates/jiji-config/src/jiji.yml`, "External Secret Adapters" section) as
supporting `adapter: doppler` with `project`/`config` fields and
`DOPPLER_TOKEN`-based auth. Confirmed by direct grep: `Config.secrets` is
parsed but never read by any runtime code path (`crates/jiji-cli/src/
env_resolution.rs`, `commands/secrets/print.rs`) -- configuring it today is a
silent no-op, not an error.

### Where it needs to plug in

- `env_resolution::resolve_secret_name` (`crates/jiji-cli/src/
  env_resolution.rs`) is the actual fallback chain today: `.env` file, then
  (if `--host-env`) the host environment. The config template's own
  documented precedence -- "`.env` values take precedence over adapter
  values" -- implies a three-tier chain: `.env` -> adapter -> host-env
  (`--host-env`). This function (or its caller, `resolve_environment`) is
  where the adapter-resolved map needs to be threaded through.
- The adapter fetch itself should happen once per `jiji deploy`/`service
  restart`/`service rollback` run (not once per secret name), likely
  alongside `load_env_file` in each command's setup, then passed down next to
  `loaded: &BTreeMap<String, String>` everywhere that's currently threaded
  (`deploy.rs`, `commands/service/restart.rs`, `commands/service/rollback.rs`
  all call `env_resolution::resolve_environment` independently today).
- `jiji secrets print` (`crates/jiji-cli/src/commands/secrets/print.rs`)
  needs to report adapter-sourced values as resolved (currently only
  `[SET]`/`[MISSING]` against `.env`/host-env), and should probably print
  which source (`.env` vs adapter vs host-env) satisfied each secret, given
  the explicit precedence order -- useful for debugging exactly the kind of
  "why did it use the wrong value" question a three-tier fallback invites.
- `registry::resolve_registry_password` (`crates/jiji-cli/src/registry.rs`)
  is the only other field CLAUDE.md documents as actually resolved from
  `.env`/host-env (`builder.registry.password`). Decide whether adapter
  support extends here too in the same pass, or is deliberately out of scope
  for a first cut -- if scoped out, say so explicitly in the same style as
  the existing `ssh.key_passphrase`, `servers.*.host`, proxy SSL
  certs, build args, and `${VAR}` interpolation visibility-only carve-outs
  documented in CLAUDE.md.

### Design notes

- Doppler resolution should run **locally**, on the machine invoking `jiji`,
  the same place `.env` files are read -- never on the remote deploy hosts.
  This matches how secrets already flow today: resolved locally, then
  uploaded via `env_resolution::stage_env_file`'s remote `--env-file`
  (never inline `-e KEY=VALUE`, so a value never appears in a logged
  command). No new remote-side code should be needed.
- Likely shells out to the `doppler` CLI (`doppler secrets download --no-file
  --format json`, scoped by `--project`/`--config` when set) rather than
  calling Doppler's HTTP API directly, consistent with this codebase's
  existing pattern of shelling out to already-installed tools (`docker`/
  `podman`/`git`) instead of vendoring API clients. Needs a clear, actionable
  error if the `doppler` binary isn't on `PATH`, and if `DOPPLER_TOKEN` is
  unset and no interactive `doppler login` session exists.
- The config template documents exactly one adapter (`doppler`) as
  supported; `SecretsAdapter.adapter` is a plain `String`, not an enum,
  suggesting the schema was intentionally left open for more adapters later
  without a breaking change. First implementation should probably still
  reject any `adapter:` value other than `"doppler"` with an actionable
  error, rather than silently no-op-ing the way the whole feature does
  today.

### Plan: build this as a pluggable adapter trait, not a Doppler-only path

The POC had exactly one adapter (Doppler). Rather than hardcoding a Doppler
call inline in `env_resolution.rs`, the Rust implementation should introduce
a small trait so later adapters (Vault, AWS/GCP Secrets Manager, 1Password,
etc.) are additive, not a rewrite:

- Define a `SecretsAdapter` trait (likely in a new `crates/jiji-config/src/
  secrets/mod.rs` or a small `jiji-secrets` crate if it grows enough to
  warrant its own dependency boundary -- e.g. shelling out needs
  `tokio::process`, which `jiji-config` doesn't otherwise depend on):
  `fn resolve(&self, keys: &[String]) -> Result<BTreeMap<String, String>>`
  (or async, matching whatever `env_resolution.rs` already needs). Keep the
  trait minimal -- "given the secret names this run needs, return what you
  have" -- so it doesn't leak Doppler-specific concepts like `project`/
  `config` into the shared interface.
- A small registry/factory function (`fn build_adapter(cfg: &SecretsAdapter)
  -> Result<Box<dyn SecretsAdapterImpl>>`, naming TBD to avoid colliding with
  the config struct's own name) matches `cfg.adapter.as_str()` against known
  adapter names and constructs the right implementation, erroring on
  anything unrecognized -- this is the one place that needs to change when a
  new adapter is added.
- `DopplerAdapter` becomes the first (and initially only) implementation:
  shells out to the `doppler` CLI as already planned above, holding
  `project`/`config` from `SecretsAdapter` and reading `DOPPLER_TOKEN` from
  the environment.
- Keep adapter-specific config fields (`project`, `config`, future adapters'
  own fields) on `SecretsAdapter` as `Option`s rather than growing the struct
  unboundedly -- if a second adapter needs materially different fields,
  revisit whether `SecretsAdapter` should become an untagged/tagged enum
  instead of one flat struct, but don't design that speculatively before a
  second real adapter exists.
- This keeps the first cut scoped to Doppler (matching what's actually
  documented and requested) while making the second adapter a matter of
  implementing the trait and registering it, not restructuring
  `env_resolution.rs`'s call sites.

### Testing approach

Doppler resolution is a **local**, non-SSH command invocation -- the
existing mock-SSH `TestServer` harness doesn't apply. Follow the pattern
already established for other local engine invocations (`registry_teardown_
test.rs`, `registry_auth_test.rs`): a fake `doppler` executable placed first
on `PATH` that logs its argv/stdin to a file for assertions, returning canned
JSON output. Also needs unit tests for the three-tier precedence in
`env_resolution.rs` (`.env` overriding adapter, adapter overriding host-env
only when `--host-env` is passed, and the existing "missing secret" error
still listing every unresolved name when even the adapter doesn't have it).

## Move `service.retain` image pruning into jiji-agent

**Status: not started, currently CLI-triggered only, no automatic equivalent.**
`jiji service prune` (`crates/jiji-cli/src/commands/service/prune.rs`) is the
only thing that ever enforces `service.retain`: for each selected endpoint's
build-configured service, `prune_service_images` lists image tags per server
over SSH (`{engine} images --format '{{.ID}}' --filter reference={repo}`,
trusting the engine's own newest-first ordering), keeps the first `retain`
(or `--retain` override), and removes the rest unless still referenced by a
running container. This only ever runs when an operator remembers to
manually invoke it -- there is no scheduled/automatic equivalent, so old
build tags accumulate on disk indefinitely between manual prune runs.

Consider moving this into `jiji-agent`'s own local reconciliation loop
(`jiji-agent/src/local_reconcile.rs::run_loop`, an infinite loop already
calling `reconcile_once` every pass with `engine`/`config: &MeshConfig` in
scope, backing off 2s-60s between passes) so pruning happens continuously
per host without operator action, the same way container/lease/proxy-route
reconciliation already does.

Gap to close first: `service.retain` (and enough registry/repo information
to run the same `images --filter reference={repo}` listing) is not currently
pushed to the agent at all -- `LocalRuntimeConfig`
(`jiji-agent/src/runtime.rs`) carries bridge/proxy/route/subnet fields only,
no per-service retain count. Would need: a new per-service field (or a small
`Vec<ServiceRetainSpec { repo, retain }>`) added to `MeshConfig`/
`LocalRuntimeConfig`, populated by `commands/server/setup.rs` the same way
`proxy_routes`/`tcp_routes` already are, plus an agent-side prune step
mirroring `prune_service_images`'s logic (list, keep first N, skip if
referenced by a running container, remove the rest). Decide whether `jiji
service prune` stays as a manual override/dry-run surface once the agent
owns the continuous version, or is retired entirely in favor of `jiji
network diagnostics`-style read-only reporting.

## Surface health-check attempt output live during `jiji deploy`'s wait

**Status: not started.** During the pre-activation health-check wait (the
`"waiting for health check (up to Ns)"` line, e.g. `Deploying
casa:postgres:postgres-0db5ed59724a: waiting for health check (up to 60s)`),
`jiji deploy` shows a single static spinner message for the entire wait --
confirmed zero interim output today. `health_check::wait_until_healthy`
(`crates/jiji-cli/src/health_check.rs`) polls the check command (`cmd`-based
exec, HTTP `curl`, or the engine-native readiness fallback, per
`plan_for_candidate`) every `plan.interval` (default 2s) until success or
`plan.deploy_timeout` elapses (default 30s, or `healthcheck.deploy_timeout`).
Each attempt's `stderr` is already captured into `last_error`, but is
silently discarded on every failed attempt and only ever surfaced once, at
the very end, bundled into `HealthCheckError::Failed { logs, .. }` (which
also calls `container_ops::logs_tail` once, only after every attempt is
exhausted). `stdout` is never captured at all, on any attempt.

Fix: update the same spinner handle already used for the static message
(`Ui::spinner(...)`'s handle, `commands/deploy.rs`, already updated
elsewhere via `report_progress`/`ctx.progress`) with the latest attempt's
captured output on each failed poll, instead of only setting the message
once at the start of the wait -- no new `Ui` primitive needed, this is an
existing pattern. Also capture `stdout` per attempt (currently dropped
entirely), since a `cmd`-based check's own diagnostic output commonly goes
to stdout, not stderr.

## Service cron: deferred v1 scope (retries, per-replica jobs, configurable retention)

**Status: not started, deliberately deferred from the v1 cron plan
(`plans/service-cron.md`).** The plan's own scope line: "The first release
does not include retries, per-replica jobs, catch-up runs, or automatic
owner failover" (`plans/service-cron.md` line 58). Manual triggering was
also considered a possible later addition during early planning, but ended
up in v1 scope and shipped in Phase 6 (`jiji service cron run`) -- it does
not need a followup item.

### Retries for failed runs

A `Failed`/`TimedOut` run is never retried on its own; the scheduler's next
tick is the only thing that runs the job again, at its next natural
schedule occurrence (`scheduler.rs::tick`). There is no configurable
per-job retry policy (max attempts, backoff). Adding one would need: a
`retry:` block on `CronConfig` (max attempts, backoff), a decision point in
`cron_exec.rs`'s `finish` (or `scheduler.rs`'s tick logic) on whether to
reschedule an immediate retry versus waiting for the next natural
occurrence, and a new `CronRunCause` variant (alongside `Scheduled`/
`Manual`) so `service cron status`/`logs` can distinguish a retry from a
normal scheduled run.

### Per-replica cron jobs

v1 always runs a service's cron job on exactly one owner -- the
lowest-ordinal Active/Healthy replica (`cron_reconcile::select_cron_owner`)
-- one execution per tick regardless of replica count. A "run on every
replica" mode (e.g. per-node cache warming) would need: a new `CronConfig`
field (e.g. `per_replica: bool`), `cron_reconcile.rs` pushing
`CronSpecApply` to every eligible replica's agent instead of just the
owner, and each replica's own scheduler claiming/running independently.
The `(service, cron_name, scheduled_at)` uniqueness constraint in
`cron_runs` is already per-agent/local-store, so concurrent independent
claims mostly just work; the real work is making `service cron
list/status/logs` replica-aware instead of reporting a single "owner" row.

### Configurable output/container retention

Retention already exists in v1, just as fixed constants, not per-service
configuration (`scheduler.rs`): completed run metadata is kept for 30 days
or the latest 100 runs per job, whichever is more (`METADATA_RETAIN_SECS`/
`METADATA_RETAIN_LATEST`); a completed run's container is kept 24 hours so
`cron logs` can still read it (`CONTAINER_RETAIN_SECS`). Making these
configurable would need a `retention:` block on `CronConfig` (e.g.
`runs`/`container_ttl`) threaded through `CronSpecApply` into
`CronJobSpec`, with `scheduler.rs`'s cleanup tick reading the per-job value
instead of the shared constants -- decide the fallback behavior (a service
without an explicit `retention:` keeps today's fixed defaults) before
implementing, so this stays additive.
