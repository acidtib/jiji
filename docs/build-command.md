# `jiji build` / `jiji deploy --build` implementation plan

## Context

`jiji deploy`, `jiji server setup`, and `jiji server teardown` are shipped.
`jiji deploy` currently bails immediately on `--build`
(`crates/jiji-cli/src/commands/deploy.rs:28-32`); there is no build/push
pipeline anywhere in the Rust codebase. This plan adds `jiji build` as a new
command and wires `deploy --build` to the same engine.

Scope, agreed with the user up front:

1. **Remote registry only.** `builder.registry.type: local` (the old POC's
   `registry:2` container + SSH port-forwarding to every deploy host) is out
   of scope — it would need a genuinely new SSH capability (local port
   forwarding) that doesn't exist in `jiji-ssh` today. Bail with an
   actionable error if `registry.kind == Local` is configured.
2. **Full multi-arch manifest assembly is in scope.** The POC's approach
   (`docker build --platform a,b` with no `--push`/`--output`) doesn't
   actually work with buildx-backed Docker — it can't load a multi-platform
   result. This plan implements real per-engine multi-platform build+push.
3. **Version tag fallback: a timestamp**, not a random ULID. Order: explicit
   `--version` (already the global `cli.version_arg`) > git short SHA (warn
   on uncommitted changes) > Unix-epoch-seconds fallback.
4. **`builder.remote`** (a dedicated SSH build server) was confirmed
   dead/unimplemented in the POC (parsed, never used) — also out of scope.
   Bail if `builder.local == false`. Builds always run on the machine
   invoking `jiji build`/`jiji deploy`.
5. **Retained-image pruning** (`service.retain`) stays deferred, same
   boundary `jiji server teardown` already drew — this plan produces and
   pushes exactly two tags (`{version}`, `latest`) and deletes nothing.

Confirmed via direct code reading (not just POC research): `tokio`'s
`"process"` feature is already enabled workspace-wide, so `local_exec.rs`
needs no new Cargo dependency. `NamedServer.arch` is parsed today but read
nowhere — this plan is what makes it load-bearing for the first time.

## Key design decisions

1. **A new local (non-SSH) command-execution primitive, mirroring
   `jiji_ssh::CommandResult`'s shape.** This is the first place in the CLI
   that runs anything on the machine invoking `jiji`, rather than over SSH.
   Two modes: `run_captured` (piped stdout/stderr/stdin, for short commands
   whose output must be inspected — `git rev-parse`, `{engine} login
   --password-stdin`, `docker buildx inspect`) and `run_streaming` (stdio
   inherited, for `build`/`buildx build`/`push`/`manifest push`, so live
   Docker/Buildx progress streams straight to the user's terminal instead of
   being buffered and replayed for a command that can run for minutes).
   Locally, args are real argv (`Command::new(program).args(args)`, no
   shell), so there's no quoting question the way there is on the SSH side.

2. **Version tag resolution separates pure precedence logic from the one
   place that shells out to git**, so it's testable without touching git or
   the wall clock (both injected).

3. **Registry image-reference and auth logic is fully pure where possible**,
   sharing a refactored password-resolution primitive with
   `env_resolution.rs` so the ALL_CAPS-vs-literal convention can't drift
   between secrets and registry passwords. This also fixes a real POC bug:
   its local-auth path used the raw config password directly (skipping env
   resolution) while only the SSH-side path resolved it. Both paths call the
   same function here.

4. **Every render function is separated from execution**, matching the
   established convention (`proxy_routes.rs`, `network_teardown.rs`,
   `container_runtime.rs`): pure `render_*`/decision functions return
   `Vec<String>`/`String` and are unit-tested with zero I/O; async wrappers
   just call `local_exec`/`SshSession::execute` and turn failure into an
   actionable `bail!`.

5. **Multi-arch detection is per-service, not a global flag.**
   `required_arches(config, service)` walks `service.hosts` → each host's
   `NamedServer.arch` (default `"amd64"`), deduped in encounter order.
   `platforms.len() <= 1` ⇒ single-arch path; `> 1` ⇒ multi-arch path. Two
   services in one run can independently be single- or multi-arch.

6. **Sequential build loop, abort-on-first-failure, by construction.**
   Unlike deploy's concurrent per-endpoint execution (which deliberately
   collects every outcome), the build loop is a plain `for` with `?`
   propagation — matches the POC's behavior and the plain reading of "build
   these one at a time."

7. **Registry login happens once per run locally, and once per unique
   deploy-target host over SSH** — not once per service/endpoint. Mirrors
   how `ensure_proxy` already runs once per server in `deploy.rs`.

8. **Scope guards fire lazily**, only when there's something to build. For
   `jiji build`: select buildable services first (may itself bail on an
   empty/unmatched selection) → *then* check `registry.kind`/`builder.local`
   → then resolve version/build. For `deploy --build`: the guards only fire
   if at least one *selected* service has `build:` configured; if `--build`
   was passed but nothing selected is buildable, that's a `Ui::warn`, not a
   bail — deploy still proceeds via `image:` for the rest.

## New files (all in `crates/jiji-cli/src/` unless noted)

### `local_exec.rs`
```rust
#[derive(Debug, Clone)]
pub struct LocalCommandResult { pub success: bool, pub stdout: String, pub stderr: String, pub code: Option<i32> }

pub async fn run_captured(program: &str, args: &[String], input: Option<&[u8]>, cwd: Option<&Path>) -> anyhow::Result<LocalCommandResult>;
// Pipes stdout/stderr; if `input` given, writes it to child stdin then drops the handle to close it.

pub async fn run_streaming(program: &str, args: &[String], cwd: Option<&Path>) -> anyhow::Result<bool>;
// Stdio inherited (live progress); stdin always Stdio::null(); returns only success/failure.

pub async fn command_exists(program: &str) -> bool;
// "{program} --version" with stdio silenced -- a fast, actionable pre-check ("docker not found")
// rather than a generic spawn error mid-build. No new `which`-style dependency.
```
Unit tests use real trivial subprocesses (`true`/`false`/`cat`/`pwd`) — no Docker required: capture success/failure, stdin piping via `cat`, `cwd` via `pwd`, `command_exists` true/false.

### `version_tag.rs`
```rust
pub struct GitStatus { pub short_sha: String, pub has_uncommitted_changes: bool }

pub fn resolve_version_tag(explicit: Option<&str>, git: Option<&GitStatus>, now_epoch_seconds: u64) -> (String, Option<String>);
// Pure. explicit > git short SHA (returns a warning string when has_uncommitted_changes, never
// prints directly -- caller does that via Ui::warn) > timestamp fallback (now_epoch_seconds.to_string()).

pub fn is_valid_docker_tag(tag: &str) -> bool; // ^[a-zA-Z0-9_][a-zA-Z0-9._-]*$, len 1..=128
pub fn validate_or_bail(tag: &str) -> anyhow::Result<()>; // catches a bad literal --version too

pub async fn gather_git_status() -> Option<GitStatus>;
// None if git isn't on PATH, cwd isn't a work tree, or there are no commits -- all "no git", not
// errors. Only the one async fn in this module touches local_exec.
```
Unit tests (pure): explicit wins over git; git+dirty → SHA + warning; git+clean → SHA, no warning;
no git → timestamp fallback equals the injected value exactly; `is_valid_docker_tag` accept/reject
table; `validate_or_bail` error names the offending value.

### `registry.rs`
```rust
const NAMESPACED_HOSTS: &[&str] = &["ghcr.io", "docker.io", "registry-1.docker.io", "index.docker.io"];

pub fn full_image_name(registry: &Registry, project: &str, service: &str, tag: &str) -> anyhow::Result<String>;
// {server}[/{username, only on a namespaced host}]/{project}-{service}:{tag}.
// Precondition (caller's job, documented not re-checked): registry.kind == Remote.

pub fn render_login_command(engine: ContainerEngine, server: &str, username: &str) -> String;
// "{engine} login {server} --username {username} --password-stdin" (single shell string, for the
// SSH path -- SshSession::execute_with_input takes one command string, matching existing convention).
pub fn render_login_args(server: &str, username: &str) -> Vec<String>; // for the local (argv) path

pub fn resolve_registry_password(raw: &str, loaded: &BTreeMap<String,String>, allow_host_env: bool) -> anyhow::Result<String>;
// Layers ALL_CAPS-vs-literal on top of env_resolution::resolve_secret_name (see below). Both
// login_local and login_remote call this same function -- fixes the POC's local-vs-remote
// resolution asymmetry by construction.

pub async fn login_local(engine: ContainerEngine, registry: &Registry, password: &str) -> anyhow::Result<()>;
pub async fn login_remote(session: &SshSession, engine: ContainerEngine, registry: &Registry, password: &str) -> anyhow::Result<()>;
```
Unit tests: namespace injection for ghcr.io/docker.io variants vs. ECR/self-hosted (no namespace);
`render_login_command`/`render_login_args` exact shape; `resolve_registry_password` ALL_CAPS
resolved-from-loaded / falls-back-to-host-env-only-when-allowed / literal-passthrough for a
non-ALL_CAPS value.

### `env_resolution.rs` (MODIFY — safe refactor, no behavior change)
Extract `resolve_environment`'s inner per-secret branch into:
```rust
pub fn resolve_secret_name(name: &str, loaded: &BTreeMap<String,String>, allow_host_env: bool) -> Option<String>;
pub fn is_bare_all_caps_name(value: &str) -> bool; // ^[A-Z][A-Z0-9_]*$
```
`resolve_environment`'s loop becomes `match resolve_secret_name(name, loaded, allow_host_env) { Some(v) => ..., None => missing.push(...) }` — all existing tests continue to pass unchanged; add one small direct test of `resolve_secret_name` for present/absent/host-env-fallback.

### `build_engine.rs` — pure command rendering + per-engine execution
```rust
pub const BUILDX_BUILDER_NAME: &str = "jiji-builder";

pub struct ResolvedBuildConfig { pub context: String, pub dockerfile: String, pub args: BTreeMap<String,String>, pub target: Option<String> }
pub fn resolve_build_config(build: &BuildValue) -> ResolvedBuildConfig; // dockerfile defaults "Dockerfile"

pub enum BuildStrategy { SingleArch, MultiArch }
pub fn build_strategy(platforms: &[String]) -> BuildStrategy; // len() <= 1 vs > 1

pub fn required_arches(config: &Config, service: &Service) -> Vec<String>;
// "linux/{arch}" per service.hosts -> NamedServer.arch (default "amd64"), deduped in encounter order.
pub fn to_platform(arch: &str) -> String; // "linux/{arch}"

pub fn multi_arch_requires_push(platforms: &[String], push: bool) -> Option<String>;
// Some(message) only when platforms.len() > 1 && !push -- buildx can't --load a multi-platform result.

fn common_build_flags(build: &ResolvedBuildConfig, no_cache: bool) -> Vec<String>;
// ["-f", dockerfile, ("--build-arg","K=V")* sorted by key, ("--target", T)?, "--no-cache"?]

pub fn render_single_arch_build(build: &ResolvedBuildConfig, no_cache: bool, tags: &[String]) -> Vec<String>;
// ["build", ...common_build_flags, "-t", tag1, "-t", tag2, context]
pub fn render_push(tag: &str) -> Vec<String>; // ["push", tag]

pub fn render_buildx_inspect() -> Vec<String>;  // ["buildx", "inspect", BUILDX_BUILDER_NAME]
pub fn render_buildx_create() -> Vec<String>;   // ["buildx","create","--name",BUILDX_BUILDER_NAME,"--driver","docker-container","--bootstrap"]
pub fn render_buildx_build(build: &ResolvedBuildConfig, no_cache: bool, platforms: &[String], tags: &[String]) -> Vec<String>;
// ["buildx","build","--builder",BUILDX_BUILDER_NAME,"--platform",platforms.join(","), ...common_build_flags, "-t",tag1,"-t",tag2,"--push",context]

pub fn manifest_name(project: &str, service: &str) -> String; // "jiji-{project}-{service}-build" (local handle, never pushed as a tag)
pub fn render_manifest_rm(name: &str) -> Vec<String>;      // ["manifest","rm",name] -- best-effort, clears stale entries from a prior build
pub fn render_manifest_create(name: &str) -> Vec<String>;  // ["manifest","create",name]
pub fn render_podman_arch_build(build: &ResolvedBuildConfig, no_cache: bool, platform: &str, manifest: &str) -> Vec<String>;
// ["build","--platform",platform, ...common_build_flags, "--manifest",manifest, context]
pub fn render_manifest_push(manifest: &str, tag: &str) -> Vec<String>;
// ["manifest","push","--all",manifest, "docker://{tag}"]

pub async fn build_and_push(engine: ContainerEngine, build: &ResolvedBuildConfig, no_cache: bool, platforms: &[String], tags: &[String], push: bool, project: &str, service_name: &str, cwd: &Path) -> anyhow::Result<()>;
```
Exact command shapes:
- **Single-arch** (Docker or Podman, identical modulo binary):
  `{engine} build [--no-cache] [--build-arg K=V]... [--target T] -f {dockerfile} -t {version_ref} -t {latest_ref} {context}`, then `{engine} push {version_ref}` + `{engine} push {latest_ref}` (only if `push`).
- **Multi-arch, Docker** (one combined command — buildx can't `--load` multi-platform, so push and manifest-list assembly happen together): `docker buildx inspect jiji-builder` (idempotency check) → `docker buildx create --name jiji-builder --driver docker-container --bootstrap` only if missing → `docker buildx build --builder jiji-builder --platform linux/amd64,linux/arm64 [--no-cache] [...] -f {dockerfile} -t {version_ref} -t {latest_ref} --push {context}`.
- **Multi-arch, Podman** (looped once per arch, pushed once per tag): `podman manifest rm jiji-{project}-{service}-build` (best-effort, ignore failure) → `podman manifest create jiji-{project}-{service}-build` → `podman build --platform linux/amd64 [...] --manifest jiji-{project}-{service}-build {context}` (repeat per arch) → `podman manifest push --all jiji-{project}-{service}-build docker://{version_ref}` + same for `{latest_ref}`.

Unit tests: `build_strategy` boundary; exact argv for `render_single_arch_build`/`render_buildx_build`/`render_podman_arch_build` (no-cache only-when-true, build-args sorted, target only-when-Some); `render_buildx_build`'s platform join preserves input order; `render_manifest_push` called once per tag; `multi_arch_requires_push` truth table; `resolve_build_config` for both `BuildValue` variants including `Detailed{dockerfile:None}` defaulting correctly; `required_arches` dedup/default/order.

### `build_plan.rs` — shared build-plan-and-execute engine (used by both `commands/build.rs` and `commands/deploy.rs`)
```rust
pub struct BuildPlanEntry { pub service_name: String, pub build: build_engine::ResolvedBuildConfig, pub platforms: Vec<String>, pub version_ref: String, pub latest_ref: String }

pub fn check_scope_guards(builder: &Builder) -> anyhow::Result<()>;
// Bails on registry.kind == Local ("not implemented yet, configure type: remote") and on
// builder.local == false ("builds always run locally; builder.remote is not implemented yet").

pub fn select_buildable_services(config: &Config, service_filters: &[String]) -> anyhow::Result<Vec<String>>;
// service.build.is_some(), filtered by -S via jiji_core::matches_pattern against the service name.
// Empty result is always an actionable error, distinguishing "no service has build: configured"
// from "-S matched no build-configured service".

pub fn compute_plan(config: &Config, project: &str, services: &[String], version_tag: &str) -> anyhow::Result<Vec<BuildPlanEntry>>;
pub fn render_plan_summary(plan: &[BuildPlanEntry]) -> String;
// "web: ghcr.io/acidtib/demo-web:abc123 [linux/amd64, linux/arm64]\n..."

pub async fn build_one(entry: &BuildPlanEntry, engine: ContainerEngine, no_cache: bool, push: bool, project_root: &Path) -> anyhow::Result<()>;
// One service, one call -- caller owns the per-service Ui::say + loop, so a failure's
// anyhow::Context naturally names the failing service and `?` naturally aborts the run.
```
Unit tests: `select_buildable_services` never selects non-build services even if `-S` names them, unmatched filter → actionable error naming it; `check_scope_guards` both bail paths; `compute_plan` — two services share one version tag but distinct basenames, mixed-arch hosts produce `platforms.len() == 2`.

### `commands/build.rs` (NEW)
```rust
pub async fn run(environment: Option<&str>, config_file: Option<&str>, services: Option<&str>, version: Option<&str>, no_cache: bool, no_push: bool, host_env: bool) -> anyhow::Result<()>;
```
1. `Ui::section("Build:")`; load + validate config (existing pattern).
2. `let push = !no_push;`
3. `build_plan::select_buildable_services` (may bail).
4. `build_plan::check_scope_guards(&config.builder)?`.
5. `version_tag::gather_git_status()` → `resolve_version_tag` → `Ui::warn` the uncommitted-changes warning if present → `validate_or_bail`.
6. `build_plan::compute_plan` → `Ui::say(render_plan_summary(...))`.
7. Pre-flight `multi_arch_requires_push` across every entry — bail before touching git/registry/Docker at all if violated (whole-run pre-flight, not a mid-run surprise on service N).
8. If `push`: load `.env`, and if `registry.username`/`password` both set, resolve password + `registry::login_local`; else `Ui::warn` "skipping login (only safe for a public registry)".
9. `Ui::section("Building:")`; sequential loop, `build_plan::build_one(...).await.with_context(...)`.
10. `Ui::section("Build Summary:")`; per-service pushed ref; `Ui::success`.

### `commands/deploy.rs` (MODIFY)
- Remove the `if build { bail }` early return.
- Hoist the existing `env_resolution::load_env_file` call to before the images loop (currently after it) — one load, reused by both the new registry-password resolution and the existing per-service secrets loop. Behavior-preserving reordering, not a logic change.
- Replace the images loop with:
  1. `services_to_build: BTreeSet<String>` = selected services where `build && service.build.is_some()`.
  2. If `build` and this set is empty: `Ui::warn("--build was passed, but no selected service has build: configured")` (non-fatal).
  3. If non-empty: `check_scope_guards`, resolve version tag (same as `build.rs`), `compute_plan`, `Ui::section("Registry Login:")` → resolve password once → `registry::login_local`.
  4. Per selected endpoint, a pure decision function (unit-tested inline, matching `container_runtime.rs`'s convention):
     ```rust
     enum ImageSource { UseImage(String), UseBuild, MissingImage, MissingImageButBuildable }
     fn resolve_service_image_source(service: &Service, build_flag: bool) -> ImageSource;
     ```
     `build_flag && build.is_some()` → `UseBuild` (build wins over a possibly-stale `image:`, even if both are set); `!build_flag && image.is_some()` → `UseImage` (unchanged); neither set → `MissingImage` (today's error, unchanged); `image.is_none() && build.is_some() && !build_flag` → `MissingImageButBuildable`, mapped to: *"Service '{name}' has no `image:` configured, but has `build:` configured. Pass `--build` to build and push it, or set `image:` directly."*
  5. `UseBuild` entries get their ref from `build_plan::build_one`'s results (built sequentially, same as `build.rs`, before finalizing the images map); `UseImage` entries keep using `container_runtime::resolve_image_reference` unchanged.
- After SSH sessions connect, before "Verifying Proxy:", add `Ui::section("Registry Login:")`: for each connected session whose server hosts at least one endpoint whose service is in `services_to_build`, `registry::login_remote` with the *same* resolved password (not re-resolved per host). Selection via a small pure function:
  ```rust
  fn hosts_serving_build_configured_services(selected: &[&ServiceEndpointPlan], services_to_build: &BTreeSet<String>) -> BTreeSet<String>;
  ```
- `no_cache: bool` threaded through the signature; if `no_cache && !build`, `Ui::warn("--no-cache has no effect without --build")` (non-fatal, informational) at the top of `run`.

### `cli.rs` (MODIFY)
```rust
Deploy {
    #[arg(long, help = "Build images before deploying")]
    build: bool,
    #[arg(long, help = "Build without using the cache (only relevant with --build)")]
    no_cache: bool,
    #[arg(long, help = "Skip kamal-proxy route activation")]
    skip_proxy: bool,
},
```
```rust
#[command(about = "Build and push images for services with `build:` configured")]
Build {
    #[arg(long, help = "Build without using the cache")]
    no_cache: bool,
    #[arg(long, overrides_with = "no_push", help = "Push the built image(s) to the registry (default)")]
    push: bool,
    #[arg(long, overrides_with = "push", help = "Build without pushing (only valid for single-architecture builds)")]
    no_push: bool,
},
```
`push`/`no_push` is the idiomatic clap-derive shape for "two named opposite flags, one effective boolean, non-`false` default": resolved as `let push = !no_push;` (the `push` field itself is never read past parsing — it exists only so `--push` parses as a no-op re-affirmation, and `overrides_with` gives last-flag-wins if a script passes both).

### `lib.rs` / `commands/mod.rs` (MODIFY)
- `mod build_engine; mod build_plan; mod local_exec; mod registry; mod version_tag;` added.
- `commands/mod.rs`: `pub mod build;`.
- `Commands::Deploy { build, no_cache, skip_proxy }` arm passes `*build, *no_cache, *skip_proxy` through in the new order.
- New `Commands::Build { no_cache, push, no_push }` arm calling `commands::build::run(cli.environment.as_deref(), cli.config_file.as_deref(), cli.services.as_deref(), cli.version_arg.as_deref(), *no_cache, *no_push, cli.host_env)`, same error-printing shape as every other arm.

## Test plan

**Pure-function unit tests** — listed per module above; covers image-reference/namespace rules,
required-arch computation, every build/push/login/buildx/podman-manifest command rendering branch,
version-tag fallback logic (fully injectable, no real git needed), and the `image:`-vs-`build:`
precedence decision. `local_exec.rs` gets a handful of real-but-trivial-subprocess tests
(`true`/`false`/`cat`/`pwd`) — no Docker required for any unit test in this plan.

**No new SSH-mock integration test for the full `--build` pipeline.** This codebase has no
dependency-injection seam for local execution today (deliberately — "plain functions, not `dyn
Trait`" is the established convention), and adding one just for test coverage would be a new
pattern introduced silently. `hosts_serving_build_configured_services` is pure and gets a plain
unit test; the rest of the SSH-visible behavior (registry login on deploy hosts, using a
build-produced ref) is covered by the live-test checklist below instead.

**Live-test checklist** (manual; ghcr.io as a placeholder push target — confirm/swap for whatever
registry the user actually wants to use before running):
1. `jiji build` single-arch/Docker — both tags pushed, `docker manifest inspect` shows one platform.
2. `jiji build` multi-arch/Docker — `jiji-builder` created via buildx, `docker buildx imagetools inspect` shows both platforms.
3. `jiji build` multi-arch/Podman — local manifest has both entries pre-push, post-push inspection confirms a real multi-platform manifest.
4. `--no-cache` — build log shows no cached layers.
5. `--no-push` single-arch — image local only, absent from the registry.
6. `--no-push` on a multi-arch-spanning service — immediate actionable bail, zero buildx/podman calls.
7. ALL_CAPS registry password (`GITHUB_TOKEN` in `.env`) — login succeeds, token never appears as a literal in a process listing (stdin-pipe verified).
8. `jiji deploy --build` full path — version resolves, build+push happens once, exactly one login per unique deploy host, the deployed container actually runs the pushed ref.
9. A service with both `image:` and `build:` set to different values, deployed with `--build` — built ref wins.
10. `jiji deploy` (no `--build`) against a `build:`-only service — new tailored "pass --build" error text.
11. Dirty working tree — warning printed, build still proceeds on the short SHA.
12. No git repo, no `--version` — timestamp fallback used, is a valid/pullable tag.
13. `registry.type: local` configured — immediate bail, zero local docker/podman commands.
14. `builder.local: false` — same immediate-bail behavior.

## Deferred (explicit, not silent)

- `builder.registry.type: local` (registry:2 container + SSH port-forwarding) — needs a new
  `jiji-ssh` capability (local port forwarding); revisit as its own plan.
- `builder.remote` (dedicated SSH build server) — confirmed dead in the POC; builds always run
  locally for this pass.
- Retained-image pruning (`service.retain`) — same boundary `server teardown` already drew; this
  plan never deletes an image.
- A dependency-injection seam for local execution (to enable a full SSH-mock end-to-end test of
  `--build`) — a viable future addition, not introduced silently here.

## Verification

- `cargo build --workspace` and `cargo test --workspace` after each module lands; `mise
  lint`/`mise fmt` clean before considering it done, matching the deploy/teardown precedent.
- New unit tests must pass under `cargo test -p jiji-cli`.
- Manual live-test checklist above, run after the automated suite is green, against a real local
  Docker and Podman install and a real registry (confirm target with the user — ghcr.io suggested
  as a default, not assumed).
