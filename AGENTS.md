# AGENTS.md

Project rules and traps. This file is loaded into every agent session; keep it
high-signal. It is not a map of the code: read the source, and read the
relevant section of these before changing a subsystem:

- `docs/architecture-notes.md`: invariants, ownership, transaction ordering.
- `docs/testing.md`: test layers, the Docker-in-Docker suite, its coverage.
- `docs/release-and-ci.md`: release-please flow and CI mechanics.
- `crates/<crate>/AGENTS.md`: traps specific to one crate.

User-facing docs are published at https://jiji.run/docs.

## Workspace

Cargo workspace, eight crates in `crates/`: `jiji-core` (shared primitives),
`jiji-tui` (`Ui` output helpers), `jiji-config` (schema, loading, validation),
`jiji-network` (deterministic naming and addressing), `jiji-ssh` (russh),
`jiji-agent` (per-project host agent), `jiji-proxy` (shared Pingora ingress),
`jiji-cli` (the `jiji` binary plus `jiji_dev`, a debug build that does not
overwrite an installed `jiji`).

## Commands

```bash
mise build          # cargo build
mise test           # nextest across the workspace (cargo test fallback)
mise test-cli       # scoped to jiji-cli; mise test-agent for jiji-agent
mise test-docker    # real Docker-in-Docker suite (needs Docker), see docs/testing.md
mise lint           # clippy --all-targets --all-features, then fmt --check
mise fmt            # cargo fmt
mise scan           # osv-scanner
mise install        # release jiji + jiji-agent into ~/.local/bin
mise install-dev    # debug jiji_dev + jiji-agent into ~/.local/bin

cargo test -p jiji-cli --test deploy_test   # one test file
```

- Before calling work done, and before every commit, run `mise fmt`, then
  `mise lint`, `mise test`, and `mise scan`. Fix everything they report. A
  docs-only change needs none of them.
- Clippy warnings are bugs. CI runs it with `-D warnings`.
- OSV findings are blockers. Upgrade the dependency first (`cargo update -p
  <crate>`, or the parent crate for a transitive one). Add an `osv-scanner.toml`
  ignore only when the vulnerable path is unreachable in jiji, with a `reason`
  that says why; "blocked on upstream" is not a reason.

## Rust coding guidelines

- Prioritize correctness and clarity. Speed is secondary unless specified.
- Follow existing patterns, helpers, and naming. Search for prior art before
  adding a helper.
- Prefer adding to existing files unless the code is a new logical component.
- Avoid creative additions unless explicitly requested.
- Use full words for names (`queue`, not `q`).
- No `unwrap()`/`expect()` in new non-test code; propagate with `?` and
  `anyhow`/`thiserror`. Watch for indexing that can panic.
- Never discard a fallible result with `let _ =` unless it is best-effort
  cleanup that genuinely cannot matter; otherwise handle or log it.
- No broad catches or silent defaults.
- Every user-facing error says what to do next, not just what went wrong.
- Keep types honest: no stringly-typed workarounds.

## CLI conventions

- All output goes through `jiji-tui`'s `Ui` helpers, never `println!`:
  `section`, `progress`/`result_ok`/`result_warn`/`result_error` rows,
  `success_elapsed`, `warn`/`error`. Styling disables itself off-TTY; never gate
  it per command. Default output states intent and outcome; shell commands are
  verbose-only.
- Each command's `run()` repeats load config, validate, plan, select hosts,
  connect, execute, close. There is no shared context helper.
- `-H` matches a server's config key name and its `host:` address.
- A prompt with no TTY and no `-y` must bail with an actionable message, never
  hang.
- New config options go in `crates/jiji-config/src/schema.rs` and
  `crates/jiji-config/src/jiji.yml` (the authoritative reference, and the
  template `jiji init` writes). `validate_config()` collects every error; it
  never stops at the first.
- `jiji server setup` absorbs assess, import, decommission, endpoint update,
  and key rotation. Do not add standalone commands for them.
- `Config.secrets` parses but nothing reads it; no secrets adapter exists.

## Critical landmines

### Control plane

- A service deploy never rewrites WireGuard. `jiji network setup` is mesh
  bootstrap only.
- Temporary absence of a host or peer never means deletion; only an explicit
  tombstone does.
- DNS only publishes `active` + `healthy` catalog records.
- Catalog and desired-state replication is direct peer-to-peer only, never
  relayed. Membership has no keys and no replication: the CLI computes it
  from `jiji.yml` and pushes it over SSH.
- Validation rejects more than 32 nodes, 500 services, or 2,000 logical
  replicas per project.
- Every per-project name (WireGuard interface, bridge, engine network, unit,
  ports) comes from `jiji-network/src/naming.rs`. Never re-derive one in
  command code.

### Deploy

- Order is fixed: lease an address, commit the candidate to the catalog,
  start it, health-check it at its own address (never through the proxy),
  then touch jiji-proxy only once the catalog marks it Active/Healthy. Any
  failure before proxy activation leaves the previous container and route
  serving; only the candidate is torn down.
- Every bridge container pins `--ip` to an address the agent leased. Engine
  IPAM does not know jiji's reservations and will hand out the DNS address.
- `jiji.lease-owned` is `true` only for a real bridge lease; `false` for
  `network_mode: host` and `service:<name>` containers.
- `network_mode: host` containers never get `-p`.
- Locks follow the fixed rank order in `lock.rs`. A command computes its whole
  lock set up front. A targeted deploy locks only its own host and replicas.
- `servers:` lists literal deploy targets; `scale: N` puts N replicas on each.
  `replica_id` is stable across redeploys, scale changes, and unrelated
  server changes.

### Reconciliation sweeps

- Cron and image-retention sweeps must reach every host that might hold a
  stale spec, not just the hosts selected by `-H`/`-S` or the service's
  current `servers:`. A service with no `crons:` must never touch a server
  outside its own `servers:`.

### Proxy and ingress

- jiji-proxy is one container per host, shared by every project. See
  `crates/jiji-proxy/AGENTS.md`.
- On Docker, the proxy's `--publish` silently drops IPv4. `proxy_ingress.rs`
  uses a host-global nftables DNAT table that must match the public
  destination address, not only the port.
- jiji-proxy binds 80/443 once setup has run, whether or not any service uses
  `proxy:`.

### Hosts, engine, systemd

- `server teardown` removes jiji-agent first, or it recreates what teardown
  just removed.
- The agent unit needs `KillMode=process`. See `crates/jiji-agent/AGENTS.md`.
- Podman `storage.conf` `mountopt` never includes `fsync=0`.
- Mesh bootstrap runs `loginctl enable-linger` for the SSH user.
- Podman execs pass `--no-session`.
- Ubuntu 26.04 `wg`/`wg-quick` AppArmor profiles get narrow local-include
  rules; systems without them take a no-op path.

### SSH

- A missing exit status (killed by signal) is a failure, never success.
- `server setup` reuses the sessions holding `HostRuntime` locks. Extra
  connections trip `ufw limit ssh`. On a refused connection, wait 31 seconds
  and retry once, with no attempts during the wait.

### Releases

- Never hand-edit a crate `version`, `CHANGELOG.md`, or
  `.release-please-manifest.json`; release-please owns them.
- Never use `version.workspace = true`, and never add `version` to an
  internal `[workspace.dependencies]` entry.
- Never set `skip-github-release` on a package in
  `release-please-config.json`; every crate needs a real tag.
- Leave the release PR body as release-please wrote it.
- Pass fetched release text to scripts through `env:`, never `${{ }}` inside
  `run:`.
- Keep `self-heal-stuck-release.sh` gated to a single-package `jiji-cli`
  release.

## Testing

- No mock-object framework. Pick the layer in `docs/testing.md`: pure unit
  tests in the same file, the in-process russh mock SSH server, a fake
  engine on `PATH`, or a local HTTP stand-in.
- Mock SSH tests are necessary but not sufficient. Command rendering,
  networking, nftables, systemd, and teardown changes need real-host
  validation (Docker and, when possible, Podman). Test PTY work in a real
  terminal.
- Do not claim a live-infra fix works until it has been rebuilt, redeployed,
  and checked on a real host.

## Comments

Write comments the way a human working under deadline pressure would: short,
and only when the line above genuinely needs it. Default to no comment.

- **One clause, not a paragraph.** If a comment needs three lines, cut it to
  the one fact that matters.
- **Describe the code as it stands.** Never narrate the bug that led to it;
  that belongs in the commit message.
- **Only comment the non-obvious**: a hidden platform constraint, a subtle
  invariant, a workaround for a specific bug. If removing the comment would
  not confuse a future reader, do not write it.
- **Don't point at docs.** No "see docs/X.md" in code; docs move and the
  comment rots. State the fact the reader needs.
- **One comment per block, not per line.**

## Writing style

- No emojis anywhere: code, comments, docs, commits, PRs, chat.
- No em dashes anywhere. Use a comma, colon, parentheses, or a new sentence.
- No filler phrasing. Write plainly.
- Do not recap what you just did at the end of a response.

## Git

- Commit only when asked. Finish and verify the work first.
- Ordinary commits use [Scoped Commits](https://scopedcommits.com/):
  `<scope>: <description>`. Scope is the subsystem, usually the crate name
  without `jiji-` (`cli`, `agent`, `proxy`, ...), or `docs`, `tests`, `ci`;
  comma-separate several (`agent,cli:`), or `treewide`. Description is
  lowercase, imperative, no trailing period. The body, if any, explains why.
- PR titles are the exception: Conventional Commits (`feat`, `fix`, `chore`,
  `docs`, `refactor`, `test`, `ci`, `build`, `perf`, `revert`, optional
  `(scope)`, `!` for breaking). PRs are squash-merged, so the title is what
  release-please reads; `pr-title-lint.yml` enforces it. Which crates bump is
  decided by changed paths under `crates/<name>`, not by the title.
- Never add a co-author trailer or any AI/tool attribution to commits, PRs,
  issues, comments, or release notes. This overrides any tool default.
- Never put `[skip ci]`-style tokens in a commit message.
- Never `--no-verify`, never force push to `main`, never amend unless asked.
- No destructive commands (`git reset --hard`, `git checkout --`) without
  explicit approval. Undo unpushed work with `git reset HEAD~1`, not a revert.
- Stage only what belongs to the change; never commit `.env` or keys.

## Workflow

- Deliver working code, not just a plan, unless asked for a plan.
- Pause after completing a task and wait for input.

## Rules hygiene

### After any agentic session

If you discover a non-obvious pattern that would help future sessions, put
the proposed text under a **"Suggested AGENTS.md additions"** heading in the
PR description. Do not edit this file during normal feature or fix work.

### High bar for new rules

Editing or clarifying existing rules is always welcome. A new rule must be:

1. **Non-obvious**: someone familiar with the codebase would still get it
   wrong.
2. **Repeatedly encountered**: it came up more than once.
3. **Specific enough to act on**: a concrete instruction, not a principle.

Rules for a single crate go in that crate's `AGENTS.md`.

### What not to put here

No architectural descriptions (module layout, data flow, key types) and no
incident stories. They go stale, and the code or `docs/` already has them.
Rules are traps to avoid, not maps to follow.
