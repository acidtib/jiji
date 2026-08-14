//! SSH-backed build execution on a `builder.remote` host. Mirrors `build_engine.rs`'s local
//! orchestration (same pure command renderers), but drives commands over an `SshSession`
//! instead of a local subprocess: preflight the builder, stage the build context, stream the
//! build/push commands, and clean up the staging directory on every exit path.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Context;
use jiji_config::{ContainerEngine, Registry};
use jiji_ssh::{ConnectOptions, RemoteForward, SshSession, StreamChunk};
use tokio::io::AsyncWriteExt;

use crate::build_context;
use crate::build_engine::{self, BuildStrategy, ResolvedBuildConfig};
use crate::build_plan::BuildPlanEntry;
use crate::{engine, env_resolution, mounts, registry};

/// Same order of magnitude as `mounts.rs`'s directory-upload cap; a build context is typically
/// larger than a mounted config directory but still bounded -- this guards against an
/// accidentally-unscoped `context: .` streaming an entire monorepo through one SSH exec.
pub const DEFAULT_MAX_CONTEXT_UPLOAD_BYTES: u64 = 512 * 1024 * 1024;

pub struct RemoteBuildExecutor {
    session: SshSession,
    staging_root: String,
    registry_forward: Option<RemoteForward>,
    engine_status: engine::EngineStatus,
}

impl RemoteBuildExecutor {
    /// Connects once, preflights the builder (installing `engine_kind` there if it's missing --
    /// see `preflight`), and creates a collision-safe staging root, all in one entry point -- a
    /// `RemoteBuildExecutor` that exists at all is ready to stage and build.
    pub async fn connect(
        connect_options: &ConnectOptions,
        engine_kind: ContainerEngine,
        project: &str,
        plan: &[BuildPlanEntry],
    ) -> anyhow::Result<Self> {
        let session = SshSession::connect(connect_options)
            .await
            .with_context(|| {
                format!(
                    "Could not connect to remote builder {}@{}:{}",
                    connect_options.user, connect_options.host, connect_options.port
                )
            })?;

        let engine_status = match preflight(&session, engine_kind, plan).await {
            Ok(status) => status,
            Err(error) => {
                session.close().await;
                return Err(error);
            }
        };

        let staging_root = match create_staging_root(&session, project).await {
            Ok(root) => root,
            Err(error) => {
                session.close().await;
                return Err(error);
            }
        };

        Ok(Self {
            session,
            staging_root,
            registry_forward: None,
            engine_status,
        })
    }

    /// Whether `connect` found `engine_kind` already installed on the builder or had to install
    /// it -- surfaced by callers (`build.rs`/`deploy.rs`) so provisioning a remote builder is
    /// visible, not silent.
    pub fn engine_status(&self) -> &engine::EngineStatus {
        &self.engine_status
    }

    /// The connected builder's own session, for a caller (`build.rs`) to write an audit entry
    /// against once the build run finishes, before `finish` consumes this executor and closes it.
    pub fn session(&self) -> &SshSession {
        &self.session
    }

    /// Prepares the builder to reach the configured registry, before any context upload. For a
    /// local registry, opens a reverse tunnel from the builder's own loopback back to the local
    /// registry (mirroring `commands/deploy.rs`'s existing tunnel-to-deploy-host pattern); a
    /// bind failure on the builder side *is* the port-conflict signal -- never stop or replace
    /// whatever else is already using that port there. For a remote registry, logs in on the
    /// builder itself (the machine that will run `push`), matching `registry::login_remote`'s
    /// existing "the machine that executes the engine authenticates" convention. `password: None`
    /// means credentials are incomplete and login should be skipped (the call site already
    /// warned); a local registry ignores `password` entirely (it's unauthenticated).
    pub async fn prepare_registry(
        &mut self,
        engine_kind: ContainerEngine,
        registry: &Registry,
        password: Option<&str>,
    ) -> anyhow::Result<()> {
        if registry.is_local() {
            let forward = self
                    .session
                    .start_reverse_forward("127.0.0.1", registry.port, registry.port)
                    .await
                    .map_err(|error| {
                        anyhow::anyhow!(
                            "Could not expose the local registry to remote builder {}: {error}. If port {} is already in use there for something else, change `builder.registry.port` instead of stopping or replacing it.",
                            self.session.host(),
                            registry.port
                        )
                    })?;
            self.registry_forward = Some(forward);
            Ok(())
        } else {
            let Some(password) = password else {
                return Ok(());
            };
            registry::login_remote(&self.session, engine_kind, registry, password).await
        }
    }

    /// Packages `build`'s context locally (off the async runtime, via `spawn_blocking`) and
    /// uploads it into a fresh, mode-0700 directory under the staging root, mirroring
    /// `mounts.rs::upload_directory`'s tar-over-stdin pattern. Returns a `ResolvedBuildConfig`
    /// with `context`/`dockerfile` rewritten to the uploaded remote paths, so it feeds straight
    /// into `build_engine`'s existing renderers unchanged.
    pub async fn stage_context(
        &self,
        service_name: &str,
        build: &ResolvedBuildConfig,
        project_root: &Path,
        engine_kind: ContainerEngine,
        max_bytes: u64,
    ) -> anyhow::Result<ResolvedBuildConfig> {
        let owned_build = build.clone();
        let owned_root = project_root.to_path_buf();
        let package = tokio::task::spawn_blocking(move || {
            build_context::package_context(&owned_root, &owned_build, engine_kind, max_bytes)
        })
        .await
        .context("Build context packaging task panicked")??;

        let remote_context = format!("{}/context/{service_name}", self.staging_root);
        let command =
            format!("set -eu; mkdir -m 0700 -p {remote_context}; tar -C {remote_context} -xf -");
        let result = self
            .session
            .execute_with_input(&command, &package.archive)
            .await?;
        mounts::ensure_success(&self.session, &command, &result)?;

        Ok(ResolvedBuildConfig {
            context: remote_context.clone(),
            dockerfile: format!("{remote_context}/{}", package.dockerfile_rel),
            args: build.args.clone(),
            target: build.target.clone(),
            secrets: build.secrets.clone(),
        })
    }

    /// Stages each resolved build secret under `{staging_root}/secrets/{service_name}/{name}`,
    /// piping content over stdin via `execute_with_input` the same way `stage_context`'s tar
    /// upload does -- no secret value is ever embedded in a command string. Unlike
    /// `env_resolution::stage_env_file`, a secret is read by `RUN --mount=type=secret` as raw
    /// bytes with no format restriction, so a multi-line value (a PEM key, a JSON credentials
    /// blob) is staged as-is with no newline rejection. No separate cleanup needed: `finish()`
    /// already `rm -rf`s the whole per-run staging root on every exit path, and this is a
    /// subdirectory of it.
    pub async fn stage_secrets(
        &self,
        service_name: &str,
        secrets: &BTreeMap<String, String>,
    ) -> anyhow::Result<Vec<(String, PathBuf)>> {
        if secrets.is_empty() {
            return Ok(Vec::new());
        }
        let remote_dir = format!("{}/secrets/{service_name}", self.staging_root);
        let mkdir_command = format!("mkdir -m 0700 -p {remote_dir}");
        let mkdir_result = self.session.execute(&mkdir_command).await?;
        mounts::ensure_success(&self.session, &mkdir_command, &mkdir_result)?;

        let mut staged = Vec::new();
        for (name, value) in secrets {
            let remote_path = format!("{remote_dir}/{name}");
            let command = format!("install -D -m 0600 /dev/stdin {remote_path}");
            let result = self
                .session
                .execute_with_input(&command, value.as_bytes())
                .await?;
            mounts::ensure_success(&self.session, &command, &result)?;
            staged.push((name.clone(), PathBuf::from(remote_path)));
        }
        Ok(staged)
    }

    /// `build` must already be remote-resolved (the output of `stage_context`); `secrets` must
    /// already be remote-staged (the output of `stage_secrets`). Mirrors
    /// `build_engine::build_and_push`'s structure over SSH instead of a local subprocess,
    /// reusing the same pure command renderers.
    #[allow(clippy::too_many_arguments)]
    pub async fn build_and_push(
        &self,
        engine_kind: ContainerEngine,
        build: &ResolvedBuildConfig,
        no_cache: bool,
        platforms: &[String],
        tags: &[String],
        push: bool,
        project: &str,
        service_name: &str,
        local_registry: bool,
        secrets: &[(String, PathBuf)],
    ) -> anyhow::Result<()> {
        if let Some(error) = build_engine::multi_arch_requires_push(platforms, push) {
            anyhow::bail!(error);
        }
        match (build_engine::build_strategy(platforms), engine_kind) {
            (BuildStrategy::SingleArch, _) => {
                self.stream(
                    engine_kind,
                    build_engine::render_single_arch_build(build, no_cache, tags, secrets),
                )
                .await?;
                if push {
                    for tag in tags {
                        self.stream(
                            engine_kind,
                            build_engine::render_push(engine_kind, local_registry, tag),
                        )
                        .await?;
                    }
                }
                Ok(())
            }
            (BuildStrategy::MultiArch, ContainerEngine::Docker) => {
                let builder_name = build_engine::buildx_builder_name(project, local_registry);
                let inspect = self
                    .capture(
                        engine_kind,
                        &build_engine::render_buildx_inspect(&builder_name),
                    )
                    .await?;
                if !inspect.success {
                    if let Err(create_error) = self
                        .stream(
                            engine_kind,
                            build_engine::render_buildx_create(&builder_name, local_registry),
                        )
                        .await
                    {
                        // Tolerate a concurrent `jiji` process winning the create race, exactly
                        // like the local path (`build_engine::build_and_push`).
                        let retry_inspect = self
                            .capture(
                                engine_kind,
                                &build_engine::render_buildx_inspect(&builder_name),
                            )
                            .await?;
                        if !retry_inspect.success {
                            return Err(create_error);
                        }
                    }
                }
                self.stream(
                    engine_kind,
                    build_engine::render_buildx_build(
                        build,
                        no_cache,
                        platforms,
                        tags,
                        &builder_name,
                        secrets,
                    ),
                )
                .await
            }
            (BuildStrategy::MultiArch, ContainerEngine::Podman) => {
                let manifest = build_engine::manifest_name(project, service_name);
                // Tolerates "doesn't exist yet" the same way the local path does: the removal's
                // own success or failure is never checked.
                let _ = self
                    .capture(engine_kind, &build_engine::render_manifest_rm(&manifest))
                    .await;
                self.stream(engine_kind, build_engine::render_manifest_create(&manifest))
                    .await?;
                for platform in platforms {
                    self.stream(
                        engine_kind,
                        build_engine::render_podman_arch_build(
                            build, no_cache, platform, &manifest, secrets,
                        ),
                    )
                    .await?;
                }
                for tag in tags {
                    self.stream(
                        engine_kind,
                        build_engine::render_manifest_push(&manifest, tag, local_registry),
                    )
                    .await?;
                }
                Ok(())
            }
        }
    }

    /// Buffered (non-streamed) command execution for control commands whose output is
    /// inspected rather than shown to the user (`buildx inspect`, `manifest rm`) -- mirrors the
    /// local path's use of `local_exec::run_captured` for the same commands.
    async fn capture(
        &self,
        engine_kind: ContainerEngine,
        args: &[String],
    ) -> anyhow::Result<jiji_ssh::CommandResult> {
        let command = shell_command(engine_kind, args);
        Ok(self.session.execute(&command).await?)
    }

    /// `rm -rf` only the staging root this invocation created -- never the parent `builds/`
    /// directory or a sibling run's staging root, so this stays safe under concurrent
    /// invocations by construction. Cancels the registry tunnel (if one was opened) before
    /// closing the session; a cancellation failure is folded into the same cleanup error rather
    /// than dropped, using `build_executor::combine_with_cleanup_error`'s same rule (primary
    /// failure wins the message, secondary failure attaches as context). Always closes the
    /// session, regardless of any cleanup outcome.
    pub async fn finish(self) -> anyhow::Result<()> {
        let command = format!("rm -rf {}", self.staging_root);
        let remove_staging = match self.session.execute(&command).await {
            Ok(result) if result.success => Ok(()),
            Ok(result) => Err(anyhow::anyhow!(
                "Could not remove remote staging directory '{}' on {}: {}",
                self.staging_root,
                self.session.host(),
                result.stderr.trim()
            )),
            Err(error) => Err(anyhow::anyhow!(
                "Could not remove remote staging directory '{}' on {}: {error}",
                self.staging_root,
                self.session.host()
            )),
        };

        let cancel_forward = if let Some(forward) = &self.registry_forward {
            self.session
                .cancel_reverse_forward(forward)
                .await
                .map_err(|error| {
                    anyhow::anyhow!(
                        "Could not cancel the registry tunnel to {}: {error}",
                        self.session.host()
                    )
                })
        } else {
            Ok(())
        };

        self.session.close().await;
        crate::build_executor::combine_with_cleanup_error(remove_staging, cancel_forward)
    }

    async fn stream(&self, engine_kind: ContainerEngine, args: Vec<String>) -> anyhow::Result<()> {
        let command = shell_command(engine_kind, &args);
        let mut receiver = self.session.execute_streaming(&command).await?;
        let mut stdout = tokio::io::stdout();
        let mut stderr = tokio::io::stderr();
        let mut exit_code = None;

        while let Some(item) = receiver.recv().await {
            match item? {
                StreamChunk::Stdout(data) => {
                    stdout.write_all(&data).await?;
                    stdout.flush().await?;
                }
                StreamChunk::Stderr(data) => {
                    stderr.write_all(&data).await?;
                    stderr.flush().await?;
                }
                StreamChunk::Exit(code) => exit_code = Some(code),
            }
        }

        match exit_code {
            Some(0) => Ok(()),
            Some(code) => anyhow::bail!(
                "Remote command '{command}' failed on {} (exit {code}).",
                self.session.host()
            ),
            None => anyhow::bail!(
                "Remote command '{command}' on {} did not report an exit status (connection closed or the command was terminated by a signal).",
                self.session.host()
            ),
        }
    }
}

fn shell_command(engine: ContainerEngine, args: &[String]) -> String {
    let mut command = engine.to_string();
    for arg in args {
        command.push(' ');
        command.push_str(&registry::shell_quote(arg));
    }
    command
}

/// Ensures the configured engine is present and new enough -- installing it if it's missing,
/// same as `jiji server setup` does for a deployment host -- and, only when `plan` actually
/// requires it, that the multi-architecture tooling it depends on is available. Multi-arch
/// tooling (Buildx/`podman manifest`) is deliberately still detect-and-report only, not
/// installed: unlike the engine itself, jiji has no distro-aware install path for it.
async fn preflight(
    session: &SshSession,
    engine_kind: ContainerEngine,
    plan: &[BuildPlanEntry],
) -> anyhow::Result<engine::EngineStatus> {
    let engine_status = engine::ensure_engine(session, engine_kind).await?;

    let needs_multi_arch = plan
        .iter()
        .any(|entry| build_engine::build_strategy(&entry.platforms) == BuildStrategy::MultiArch);
    if !needs_multi_arch {
        return Ok(engine_status);
    }

    match engine_kind {
        ContainerEngine::Docker => {
            let result = session.execute("docker buildx version").await?;
            if !result.success {
                anyhow::bail!(
                    "Remote builder {} does not have Docker Buildx available, which the configured multi-architecture build requires. Install the buildx plugin on the builder host and retry.",
                    session.host()
                );
            }
        }
        ContainerEngine::Podman => {
            let result = session.execute("podman manifest --help").await?;
            if !result.success {
                anyhow::bail!(
                    "Remote builder {} does not support `podman manifest`, which the configured multi-architecture build requires. Upgrade Podman on the builder host and retry.",
                    session.host()
                );
            }
        }
    }
    Ok(engine_status)
}

async fn create_staging_root(session: &SshSession, project: &str) -> anyhow::Result<String> {
    let builds_dir = format!("{}/builds", env_resolution::project_staging_dir(project));
    let command = format!("set -eu; mkdir -p {builds_dir}; mktemp -d {builds_dir}/run.XXXXXX");
    let result = session.execute(&command).await?;
    mounts::ensure_success(session, &command, &result)?;

    let staging_root = result.stdout.trim();
    if !staging_root.starts_with(&format!("{builds_dir}/run."))
        || staging_root.contains(char::is_whitespace)
    {
        anyhow::bail!(
            "Remote builder {} returned an unsafe staging path '{staging_root}'.",
            session.host()
        );
    }
    Ok(staging_root.to_string())
}
