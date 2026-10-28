# Releases and CI

Mechanics of the release-please flow and the CI workflow. The rules agents
must not break are summarized in `AGENTS.md` under "Critical landmines".

## Versions

All 8 crates have an explicit `[package].version`, never
`version.workspace = true`: the `cargo-workspace` release-please plugin
hard-fails on any workspace member using it, since it scans every
`Cargo.toml` under `[workspace].members` regardless of release-please's own
`packages` config. There is no repo-wide version and no manual bump command.
Never hand-edit a crate's `version` or `.release-please-manifest.json`
outside a release-please PR.

Config: `release-please-config.json`, `.release-please-manifest.json`,
`.github/workflows/release-please.yml`, `.github/workflows/release.yml`.

## Release flow

Nothing is tagged before CI passes on it.

- `release-please.yml` (every push to `main`) only opens or updates the
  release PR (action input `skip-github-release: true`, a per-run switch
  unrelated to the per-package config option below) and keeps a summary
  comment on it (`.github/scripts/release-pr-summary.py`). The PR body must
  stay as release-please wrote it, since release-please parses it again to
  create the tags.
- `release.yml` runs on CI completing (`workflow_run`) and only acts when CI
  succeeded on a push of a release commit (message starting
  `chore: release`). It self-heals, runs release-please in tag-only mode
  (`skip-github-pull-request: true`), then calls the build/publish workflow
  for each tag created, in the same run. Nothing triggers on tag pushes.
- While a merged release PR is still untagged (its CI running),
  release-please refuses to open or update a release PR; it catches up on
  the next push. If a release commit's CI fails, re-run it (a successful
  re-run triggers `release.yml` again) or dispatch `release.yml` by hand.
- Both workflows authenticate with a GitHub App (`RELEASE_APP_CLIENT_ID`
  variable, `RELEASE_APP_PRIVATE_KEY` secret), never a personal token.

## Tags and published artifacts

All 8 crates get an independent tag (`vX.Y.Z` for `jiji`,
`{package}-vX.Y.Z` for the rest) and a GitHub Release. Only `jiji`,
`jiji-agent`, and `jiji-proxy` have a build/publish workflow
(`jiji-release.yml`, `jiji-agent-release.yml`, `jiji-proxy-release.yml`,
called by `release.yml` or dispatched by hand to rebuild an existing tag).
The other 5 are internal-only; their tag exists purely as a release-please
anchor.

`cargo-workspace` patch-bumps every crate that depends on whatever bumped, so
a `fix:` to `jiji-core` cascades through `jiji-network` into
`jiji-cli`/`jiji-agent` with no `Release-As:` footer. `jiji-proxy` has no
internal deps, so nothing cascades into it.

`jiji-cli`'s and `jiji-network`'s `build.rs` read a sibling crate's version at
compile time (`jiji-agent` to `AGENT_BUILD_VERSION`, `jiji-proxy` to
`PROXY_VERSION`/`image()`) via `sibling_crate_version()`, `include!`-ed from
`lib/build-support/sibling_crate_version.rs`, because `jiji server setup`
cannot assume the agent/proxy build to install is the CLI's own version.

## Gotchas (all confirmed live)

**`skip-github-release` per package means no tag.** Without a tag,
release-please has no anchor, so every run re-surfaces the original commit
and bumps that package again, forever, cascading empty patch bumps onto every
dependent (looped through 3 release PRs before being caught). Every package
must get a real tag; the build/publish workflow's presence is what
distinguishes consumable from internal-only.

**Internal `[workspace.dependencies]` entries stay bare `{ path = "..." }`.**
A `version` field makes cargo semver-check it against the crate's real
version; crates bump independently (`bump-minor-pre-major: true`), so the
next minor bump breaks the workspace build. It would not help changelogs
either: release-please's `CargoToml` updater skips any dep using
`dep.workspace = true`.

Because of that, `.github/scripts/expand-dependency-changelog.sh
<package-name>` writes dependency changelog notes itself: it diffs
`.release-please-manifest.json` against `HEAD~1`, filters to the releasing
package's deps (`cargo metadata --no-deps`), and appends each bumped crate's
own `CHANGELOG.md` section under "### Crate changes in this release". Always
pipe a fetched release body into it via `env:`, never interpolate `${{ }}`
into a `run:` block; it is free-text commit/PR content.

**The self-heal script must stay gated.**
`.github/scripts/self-heal-stuck-release.sh` (runs in `release.yml` before
the tag step and reports created tags as a step output) recovers a release PR
that release-please's `getBranchComponent`/`getComponent` bug (upstream
#2214) drops. That bug only occurs when the PR released exactly one package
and that package has `include-component-in-tag: false` (only `jiji-cli`
here). A release PR is always still labeled `autorelease: pending` just
before release-please-action processes it, so that label is not evidence of
being stuck. An ungated version pre-created all 5 tags for a normal
5-package release, and release-please-action then failed with
`Release already exists`.

## CI (`.github/workflows/ci.yml`)

- `changes` runs `.github/scripts/ci-changes.sh` and decides what the rest
  needs. On a PR, a change confined to crates runs `test` with a nextest
  `rdeps(<crate>)` filter. A change to `Cargo.toml`/`Cargo.lock`, `.config/`,
  `lib/`, `test/`, or CI itself runs everything, and so does every push.
  Docs-only changes skip Rust work. Run locally with
  `EVENT_NAME=pull_request BASE_REF=main .github/scripts/ci-changes.sh`.
- `build` compiles once into a nextest archive plus `jiji-agent`. `test` and
  the three `docker-in-docker tests (N/3)` shards (`--partition slice:N/3`,
  one vm1/vm2 per runner; `count:` would put every single-test docker binary
  on shard 1) extract it with `--extract-to "$PWD"` so compile-time
  `env!("CARGO_BIN_EXE_jiji")` paths resolve. The archive does not carry
  `jiji-agent`, so docker shards get it as a separate artifact.
- `proxy-image` builds `jiji-proxy` from the checkout whenever the docker
  suite runs and the change touches `crates/jiji-proxy/`,
  `Cargo.toml`/`Cargo.lock`, or `test/docker/`, or the proxy version is not
  on ghcr.io yet. Each shard pushes it to the compose `registry` service
  (`127.0.0.1:5099`), and vm1/vm2 mirror `ghcr.io/acidtib/jiji-proxy` to it
  (`test/docker/vm/registries.conf`), falling back to ghcr.io on a miss.
  Without the mirror, a release commit that bumps `jiji-proxy` deadlocks:
  setup pulls an image only published after tagging, and tagging waits on
  this suite. `mise test-docker` builds and pushes the image the same way.
- Docker shards pin `runs-on: ubuntu-24.04`. On an Ubuntu 26.04 runner the
  runner kernel's own AppArmor `wg`/`wg-quick` profiles confine `wg` inside
  the privileged vm1/vm2, and jiji's local-include fix cannot reach the
  runner's profiles.
- `clippy` (`-D warnings`), `clippy-macos` (`jiji-cli`), `fmt`, `deps`
  (`cargo update --locked`, `cargo shear`, `osv-scanner` against
  `osv-scanner.toml`), and `workflows` (actionlint, shellcheck, zizmor with
  `.github/zizmor.yml`) run alongside.
- `ci-pass` is the single required check: it fails unless every job
  succeeded or was skipped. Every job sets `timeout-minutes`; every
  third-party action is pinned to a commit SHA (`@<sha> # vX.Y.Z`).
  `dtolnay/rust-toolchain` pinned by SHA needs an explicit `toolchain:`
  input.
- Runner usage: release-please PRs skip everything; on `main` the docker
  shards run only for release commits; a newer push cancels an older run
  except a release commit's (own concurrency group); closing a PR cancels
  its run; only `main` saves the cargo cache.
