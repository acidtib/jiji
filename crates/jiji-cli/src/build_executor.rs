//! Where a build actually runs. `Local` wraps today's unchanged `build_engine::build_and_push`
//! (a local subprocess); `Remote` owns a connected `RemoteBuildExecutor` (an SSH session, a
//! staging root, and everything needed to stage a context and stream a build over it). Exactly
//! one is selected per invocation from `build_plan::select_executor`'s `ExecutorTarget`.

use std::path::Path;

use jiji_config::{ContainerEngine, Registry};
use jiji_ssh::SshSession;

use crate::build_plan::{BuildPlanEntry, ExecutorTarget};
use crate::{build_engine, engine, registry, remote_build};

pub enum BuildExecutor {
    Local,
    Remote(Box<remote_build::RemoteBuildExecutor>),
}

impl BuildExecutor {
    /// For `Remote`, connects and preflights against `plan` up front -- a broken or
    /// under-provisioned builder fails here, before any service's build starts, rather than
    /// partway through the loop.
    pub async fn prepare(
        target: ExecutorTarget,
        engine: ContainerEngine,
        project: &str,
        plan: &[BuildPlanEntry],
    ) -> anyhow::Result<Self> {
        match target {
            ExecutorTarget::Local => Ok(BuildExecutor::Local),
            ExecutorTarget::Remote { connect_options } => {
                let remote = remote_build::RemoteBuildExecutor::connect(
                    &connect_options,
                    engine,
                    project,
                    plan,
                )
                .await?;
                Ok(BuildExecutor::Remote(Box::new(remote)))
            }
        }
    }

    /// `None` for `Local` (no remote engine to provision); `Some` for `Remote`, reflecting
    /// whether `prepare` found the builder's engine already installed or had to install it.
    pub fn engine_status(&self) -> Option<&engine::EngineStatus> {
        match self {
            BuildExecutor::Local => None,
            BuildExecutor::Remote(remote) => Some(remote.engine_status()),
        }
    }

    /// The remote builder's own SSH session, for `build.rs` to write a `build` audit entry
    /// against once the run finishes. `None` for `Local`: a local build never opens a session,
    /// so there is no host to audit against (same local-only exclusion `registry teardown` and
    /// local `registry login`/`logout` already use).
    pub fn remote_session(&self) -> Option<&SshSession> {
        match self {
            BuildExecutor::Local => None,
            BuildExecutor::Remote(remote) => Some(remote.session()),
        }
    }

    /// Prepares the executor to reach the configured registry, before any context upload.
    /// `Local` reuses today's unchanged `registry::login_local` (a no-op for a local registry,
    /// which needs no authentication); `Remote` delegates to
    /// `RemoteBuildExecutor::prepare_registry` (local-registry tunnel, or login on the builder
    /// itself for a remote registry). `password: None` means credentials are incomplete and
    /// login should be skipped -- the call site already warned about that.
    pub async fn prepare_registry(
        &mut self,
        engine: ContainerEngine,
        registry_config: &Registry,
        password: Option<&str>,
    ) -> anyhow::Result<()> {
        match self {
            BuildExecutor::Local => {
                if !registry_config.is_local() {
                    if let Some(password) = password {
                        registry::login_local(engine, registry_config, password).await?;
                    }
                }
                Ok(())
            }
            BuildExecutor::Remote(remote) => {
                remote
                    .prepare_registry(engine, registry_config, password)
                    .await
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn build_and_push(
        &self,
        entry: &BuildPlanEntry,
        engine: ContainerEngine,
        no_cache: bool,
        push: bool,
        project: &str,
        project_root: &Path,
        local_registry: bool,
    ) -> anyhow::Result<()> {
        let tags = [entry.version_ref.clone(), entry.latest_ref.clone()];
        match self {
            BuildExecutor::Local => {
                build_engine::build_and_push(
                    engine,
                    &entry.build,
                    no_cache,
                    &entry.platforms,
                    &tags,
                    push,
                    project,
                    &entry.service_name,
                    project_root,
                    local_registry,
                )
                .await
            }
            BuildExecutor::Remote(remote) => {
                let remote_build = remote
                    .stage_context(
                        &entry.service_name,
                        &entry.build,
                        project_root,
                        engine,
                        remote_build::DEFAULT_MAX_CONTEXT_UPLOAD_BYTES,
                    )
                    .await?;
                remote
                    .build_and_push(
                        engine,
                        &remote_build,
                        no_cache,
                        &entry.platforms,
                        &tags,
                        push,
                        project,
                        &entry.service_name,
                        local_registry,
                    )
                    .await
            }
        }
    }

    /// No-op for `Local`. For `Remote`, removes the staging root and closes the SSH session --
    /// see `combine_with_cleanup_error` for how callers should fold this into the build loop's
    /// own result.
    pub async fn finish(self) -> anyhow::Result<()> {
        match self {
            BuildExecutor::Local => Ok(()),
            BuildExecutor::Remote(remote) => remote.finish().await,
        }
    }
}

/// Finally-block combinator: cleanup always runs, but the primary error (the actual build
/// failure) takes precedence in what's reported. A cleanup failure is never silently dropped --
/// on a double failure it's attached to the primary error as additional context.
pub fn combine_with_cleanup_error(
    primary: anyhow::Result<()>,
    cleanup: anyhow::Result<()>,
) -> anyhow::Result<()> {
    match (primary, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(primary_error), Ok(())) => Err(primary_error),
        (Ok(()), Err(cleanup_error)) => Err(cleanup_error),
        (Err(primary_error), Err(cleanup_error)) => {
            Err(primary_error.context(format!("additionally, cleanup failed: {cleanup_error}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_ok_is_ok() {
        assert!(combine_with_cleanup_error(Ok(()), Ok(())).is_ok());
    }

    #[test]
    fn primary_failure_alone_is_reported() {
        let error =
            combine_with_cleanup_error(Err(anyhow::anyhow!("build failed")), Ok(())).unwrap_err();
        assert_eq!(error.to_string(), "build failed");
    }

    #[test]
    fn cleanup_failure_alone_is_reported() {
        let error =
            combine_with_cleanup_error(Ok(()), Err(anyhow::anyhow!("cleanup failed"))).unwrap_err();
        assert_eq!(error.to_string(), "cleanup failed");
    }

    #[test]
    fn both_failing_keeps_the_primary_message_and_attaches_cleanup_context() {
        let error = combine_with_cleanup_error(
            Err(anyhow::anyhow!("build failed")),
            Err(anyhow::anyhow!("cleanup failed")),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "additionally, cleanup failed: cleanup failed"
        );
        assert!(error
            .chain()
            .any(|cause| cause.to_string() == "build failed"));
    }
}
