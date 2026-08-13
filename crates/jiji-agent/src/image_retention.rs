//! Continuous, autonomous image-tag retention: durable local state for `jiji-agent`'s per-service
//! image pruning, plus the local pruning itself. Unlike `CronJobSpec`, an installed spec has no
//! single owner -- `jiji-cli` pushes it identically to every host in a service's eligible
//! `servers:` set (see `image_retention_reconcile.rs` there), since each host prunes its own
//! independent local image cache. Runs the engine binary directly with an explicit argv (never a
//! shell string), mirroring `discovery.rs`'s stated injection-avoidance precedent.

use std::collections::BTreeSet;

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
}

/// Lists image `(id, repo:tag)` pairs for `repo` and removes every one after the first
/// `retain_n`, skipping any still referenced by a container. Deliberately relies on the engine's
/// own `images` listing order (newest first for both Docker and Podman) rather than parsing
/// `CreatedAt` -- the same assumption `jiji-cli`'s `commands/service/prune.rs::prune_service_images`
/// documents and relies on for the manual-override path. A freshly pulled candidate image is
/// always the newest entry in that listing, so it stays inside the retained window purely by
/// recency for any `retain_n >= 1` even in the narrow gap between "image pulled" and "candidate
/// container created" where the reference check alone wouldn't yet protect it; only
/// `retain_n == 0` loses this property, which already carries the same risk today via manual
/// `jiji service prune --retain 0`.
///
/// Returns only per-image failures as `(image_id, error)` pairs: removals and retained skips are
/// not surfaced, since the sole caller (`local_reconcile::fold_retention_problems`) only reports
/// what went wrong.
pub async fn prune_repo(
    engine: Engine,
    repo: &str,
    retain_n: usize,
) -> Result<Vec<(String, String)>, String> {
    prune_repo_with_binary(engine.as_str(), engine, repo, retain_n).await
}

async fn prune_repo_with_binary(
    binary: &str,
    engine: Engine,
    repo: &str,
    retain_n: usize,
) -> Result<Vec<(String, String)>, String> {
    let images = list_images(binary, repo).await?;
    // One `ps -a` snapshot for the whole run rather than a per-candidate `ancestor=` probe: the
    // reconcile loop reruns every tick, so probing per image would spawn one engine process per
    // candidate per tick. Matching by repo:tag (the name every jiji container is created with)
    // instead of the `ancestor` filter's wider "image or any of its ancestors" match is safe:
    // `rmi` refuses any directly-referenced image itself, so the wider match only ever
    // pre-skipped removals that would have failed anyway and been reported back.
    let in_use: BTreeSet<String> = list_container_images(binary).await?.into_iter().collect();
    let mut failures = Vec::new();
    for (id, name) in images.into_iter().skip(retain_n) {
        if in_use.contains(&name) {
            continue;
        }
        if let Some(error) = remove_image(binary, engine, &id).await {
            failures.push((id, error));
        }
    }
    Ok(failures)
}

async fn list_images(binary: &str, repo: &str) -> Result<Vec<(String, String)>, String> {
    let output = tokio::process::Command::new(binary)
        .arg("images")
        .arg("--format")
        .arg("{{.ID}} {{.Repository}}:{{.Tag}}")
        .arg("--filter")
        .arg(format!("reference={repo}"))
        .output()
        .await
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            let (id, name) = line
                .split_once(' ')
                .ok_or_else(|| format!("unexpected images output line: {line:?}"))?;
            Ok((id.to_string(), name.to_string()))
        })
        .collect()
}

async fn list_container_images(binary: &str) -> Result<Vec<String>, String> {
    let output = tokio::process::Command::new(binary)
        .arg("ps")
        .arg("-a")
        .arg("--format")
        .arg("{{.Image}}")
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

/// `None` when the image was removed or already absent; `Some(error)` only for a real removal
/// failure worth reporting.
async fn remove_image(binary: &str, engine: Engine, image_id: &str) -> Option<String> {
    let output = tokio::process::Command::new(binary)
        .arg("rmi")
        .arg(image_id)
        .output()
        .await;
    match output {
        Ok(output) if output.status.success() => None,
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if is_missing_image_error(&stderr) {
                None
            } else {
                Some(stderr)
            }
        }
        Err(error) => {
            warn!(%error, %engine, image_id, "could not run image removal");
            Some(error.to_string())
        }
    }
}

/// Docker and Podman phrase "no such image" differently; a `false`-negative here just means an
/// already-gone image is reported as a failure instead of being skipped, never the reverse.
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

    fn rmi_log(dir: &std::path::Path) -> String {
        std::fs::read_to_string(dir.join("rmi.log")).unwrap_or_default()
    }

    fn logging_engine(
        dir: &std::path::Path,
        images: &[&str],
        containers: &[&str],
    ) -> std::path::PathBuf {
        let lines = |entries: &[&str]| {
            let mut body = entries
                .iter()
                .map(|entry| format!("  printf '%s\\n' '{entry}'\n"))
                .collect::<String>();
            // dash rejects an empty then-branch immediately followed by `elif`.
            if body.is_empty() {
                body.push_str("  :\n");
            }
            body
        };
        fake_engine(
            dir,
            &format!(
                r#"#!/bin/sh
if [ "$1" = "images" ]; then
{images}elif [ "$1" = "ps" ]; then
{containers}elif [ "$1" = "rmi" ]; then
  echo "$2" >> {log}
  exit 0
fi
"#,
                images = lines(images),
                containers = lines(containers),
                log = dir.join("rmi.log").display(),
            ),
        )
    }

    #[tokio::test]
    async fn nothing_removed_when_under_the_retain_count() {
        let dir = tempfile::tempdir().unwrap();
        let engine = fake_engine(
            dir.path(),
            r#"#!/bin/sh
if [ "$1" = "images" ]; then
  printf 'img1 demo/web:v1\nimg2 demo/web:v2\n'
fi
"#,
        );
        let failures =
            prune_repo_with_binary(engine.to_str().unwrap(), Engine::Docker, "demo/web", 3)
                .await
                .unwrap();
        assert!(failures.is_empty());
    }

    #[tokio::test]
    async fn oldest_beyond_retain_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let engine = logging_engine(
            dir.path(),
            &["img1 demo/web:v1", "img2 demo/web:v2", "img3 demo/web:v3"],
            &[],
        );
        let failures =
            prune_repo_with_binary(engine.to_str().unwrap(), Engine::Docker, "demo/web", 2)
                .await
                .unwrap();
        assert!(failures.is_empty());
        assert_eq!(rmi_log(dir.path()), "img3\n");
    }

    #[tokio::test]
    async fn a_still_referenced_image_beyond_retain_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let engine = logging_engine(
            dir.path(),
            &["img1 demo/web:v1", "img2 demo/web:v2"],
            &["demo/web:v2"],
        );
        let failures =
            prune_repo_with_binary(engine.to_str().unwrap(), Engine::Docker, "demo/web", 1)
                .await
                .unwrap();
        assert!(failures.is_empty());
        assert_eq!(rmi_log(dir.path()), "");
    }

    #[tokio::test]
    async fn a_mix_of_retained_and_removed_in_one_run() {
        let dir = tempfile::tempdir().unwrap();
        let engine = logging_engine(
            dir.path(),
            &["img1 demo/web:v1", "img2 demo/web:v2", "img3 demo/web:v3"],
            &["demo/web:v2"],
        );
        let failures =
            prune_repo_with_binary(engine.to_str().unwrap(), Engine::Docker, "demo/web", 1)
                .await
                .unwrap();
        assert!(failures.is_empty());
        assert_eq!(rmi_log(dir.path()), "img3\n");
    }

    #[tokio::test]
    async fn a_failed_rmi_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let engine = fake_engine(
            dir.path(),
            r#"#!/bin/sh
if [ "$1" = "images" ]; then
  printf 'img1 demo/web:v1\nimg2 demo/web:v2\n'
elif [ "$1" = "ps" ]; then
  exit 0
elif [ "$1" = "rmi" ]; then
  echo "image is referenced in multiple repositories" >&2
  exit 1
fi
"#,
        );
        let failures =
            prune_repo_with_binary(engine.to_str().unwrap(), Engine::Docker, "demo/web", 1)
                .await
                .unwrap();
        assert_eq!(
            failures,
            vec![(
                "img2".to_string(),
                "image is referenced in multiple repositories".to_string()
            )]
        );
    }
}
