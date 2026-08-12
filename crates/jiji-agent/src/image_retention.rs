//! Continuous, autonomous image-tag retention: durable local state for `jiji-agent`'s per-service
//! image pruning, plus the local pruning itself. Unlike `CronJobSpec`, an installed spec has no
//! single owner -- `jiji-cli` pushes it identically to every host in a service's eligible
//! `servers:` set (see `image_retention_reconcile.rs` there), since each host prunes its own
//! independent local image cache. Runs the engine binary directly with an explicit argv (never a
//! shell string), mirroring `discovery.rs`'s stated injection-avoidance precedent.

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::engine::Engine;

/// One installed image-retention specification, built by `jiji-cli` from a successful service
/// deployment and pushed to every host in the service's eligible `servers:` set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageRetentionSpec {
    pub service: String,
    pub repo: String,
    pub retain: u32,
    /// Bumped by the CLI on every push; used with `repo`/`retain` for `ImageRetentionApply`'s
    /// idempotent-upsert contract (see `AgentStore::apply_image_retention_spec`).
    pub revision: u64,
}

/// Outcome of `AgentStore::apply_image_retention_spec`'s idempotent upsert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageRetentionApplyOutcome {
    Installed(ImageRetentionSpec),
    /// An existing spec for this service had a different `repo` and/or `retain`; replaced.
    Updated(ImageRetentionSpec),
    /// An existing spec already matched both `repo` and `retain`; left untouched.
    Unchanged(ImageRetentionSpec),
}

impl ImageRetentionApplyOutcome {
    pub fn spec(&self) -> &ImageRetentionSpec {
        match self {
            ImageRetentionApplyOutcome::Installed(spec)
            | ImageRetentionApplyOutcome::Updated(spec)
            | ImageRetentionApplyOutcome::Unchanged(spec) => spec,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PruneOutcome {
    Removed,
    AlreadyAbsent,
    Retained { by: Vec<String> },
    Failed { error: String },
}

/// Lists image IDs for `repo` and removes every one after the first `retain_n`, skipping any
/// still referenced by a container. Deliberately relies on the engine's own `images` listing
/// order (newest first for both Docker and Podman) rather than parsing `CreatedAt` -- the same
/// assumption `jiji-cli`'s `commands/service/prune.rs::prune_service_images` documents and relies
/// on for the manual-override path. A freshly pulled candidate image is always the newest entry
/// in that listing, so it stays inside the retained window purely by recency for any `retain_n >=
/// 1` even in the narrow gap between "image pulled" and "candidate container created" where the
/// reference check alone wouldn't yet protect it; only `retain_n == 0` loses this property, which
/// already carries the same risk today via manual `jiji service prune --retain 0`.
pub async fn prune_repo(
    engine: Engine,
    repo: &str,
    retain_n: usize,
) -> Result<Vec<(String, PruneOutcome)>, String> {
    prune_repo_with_binary(engine.as_str(), engine, repo, retain_n).await
}

async fn prune_repo_with_binary(
    binary: &str,
    engine: Engine,
    repo: &str,
    retain_n: usize,
) -> Result<Vec<(String, PruneOutcome)>, String> {
    let ids = list_image_ids(binary, repo).await?;

    let mut outcomes = Vec::new();
    for id in ids.into_iter().skip(retain_n) {
        let referenced = referencing_containers(binary, &id).await?;
        if !referenced.is_empty() {
            outcomes.push((id, PruneOutcome::Retained { by: referenced }));
            continue;
        }
        outcomes.push((id.clone(), remove_image(binary, engine, &id).await));
    }
    Ok(outcomes)
}

async fn list_image_ids(binary: &str, repo: &str) -> Result<Vec<String>, String> {
    let output = tokio::process::Command::new(binary)
        .arg("images")
        .arg("--format")
        .arg("{{.ID}}")
        .arg("--filter")
        .arg(format!("reference={repo}"))
        .output()
        .await
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

async fn referencing_containers(binary: &str, image_id: &str) -> Result<Vec<String>, String> {
    let output = tokio::process::Command::new(binary)
        .arg("ps")
        .arg("-a")
        .arg("--filter")
        .arg(format!("ancestor={image_id}"))
        .arg("--format")
        .arg("{{.Names}}")
        .output()
        .await
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect())
}

async fn remove_image(binary: &str, engine: Engine, image_id: &str) -> PruneOutcome {
    let output = tokio::process::Command::new(binary)
        .arg("rmi")
        .arg(image_id)
        .output()
        .await;
    match output {
        Ok(output) if output.status.success() => PruneOutcome::Removed,
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if is_missing_image_error(&stderr) {
                PruneOutcome::AlreadyAbsent
            } else {
                PruneOutcome::Failed { error: stderr }
            }
        }
        Err(error) => {
            warn!(%error, %engine, image_id, "could not run image removal");
            PruneOutcome::Failed {
                error: error.to_string(),
            }
        }
    }
}

/// Docker and Podman phrase "no such image" differently; a `false`-negative here just means an
/// already-gone image is reported as a failure instead of `AlreadyAbsent`, never the reverse.
fn is_missing_image_error(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    lower.contains("no such image") || lower.contains("image not known")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn fake_engine(dir: &std::path::Path, script: &str) -> std::path::PathBuf {
        let path = dir.join("fake-engine");
        std::fs::write(&path, script).expect("write fake engine");
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    }

    #[tokio::test]
    async fn nothing_removed_when_under_the_retain_count() {
        let dir = tempfile::tempdir().unwrap();
        let engine = fake_engine(
            dir.path(),
            r#"#!/bin/sh
if [ "$1" = "images" ]; then
  printf 'img1\nimg2\n'
fi
"#,
        );
        let outcomes =
            prune_repo_with_binary(engine.to_str().unwrap(), Engine::Docker, "demo/web", 3)
                .await
                .unwrap();
        assert!(outcomes.is_empty());
    }

    #[tokio::test]
    async fn oldest_beyond_retain_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let engine = fake_engine(
            dir.path(),
            r#"#!/bin/sh
if [ "$1" = "images" ]; then
  printf 'img1\nimg2\nimg3\n'
elif [ "$1" = "ps" ]; then
  exit 0
elif [ "$1" = "rmi" ]; then
  exit 0
fi
"#,
        );
        let outcomes =
            prune_repo_with_binary(engine.to_str().unwrap(), Engine::Docker, "demo/web", 2)
                .await
                .unwrap();
        assert_eq!(outcomes, vec![("img3".to_string(), PruneOutcome::Removed)]);
    }

    #[tokio::test]
    async fn a_still_referenced_image_beyond_retain_is_retained_with_the_container_name() {
        let dir = tempfile::tempdir().unwrap();
        let engine = fake_engine(
            dir.path(),
            r#"#!/bin/sh
if [ "$1" = "images" ]; then
  printf 'img1\nimg2\n'
elif [ "$1" = "ps" ]; then
  printf 'demo-web-abc123\n'
fi
"#,
        );
        let outcomes =
            prune_repo_with_binary(engine.to_str().unwrap(), Engine::Docker, "demo/web", 1)
                .await
                .unwrap();
        assert_eq!(
            outcomes,
            vec![(
                "img2".to_string(),
                PruneOutcome::Retained {
                    by: vec!["demo-web-abc123".to_string()]
                }
            )]
        );
    }

    #[tokio::test]
    async fn a_mix_of_retained_and_removed_in_one_run() {
        let dir = tempfile::tempdir().unwrap();
        let engine = fake_engine(
            dir.path(),
            r#"#!/bin/sh
if [ "$1" = "images" ]; then
  printf 'img1\nimg2\nimg3\n'
elif [ "$1" = "ps" ]; then
  case "$4" in
    *img2*) printf 'still-running\n' ;;
    *) exit 0 ;;
  esac
elif [ "$1" = "rmi" ]; then
  exit 0
fi
"#,
        );
        let outcomes =
            prune_repo_with_binary(engine.to_str().unwrap(), Engine::Docker, "demo/web", 1)
                .await
                .unwrap();
        assert_eq!(
            outcomes,
            vec![
                (
                    "img2".to_string(),
                    PruneOutcome::Retained {
                        by: vec!["still-running".to_string()]
                    }
                ),
                ("img3".to_string(), PruneOutcome::Removed),
            ]
        );
    }
}
