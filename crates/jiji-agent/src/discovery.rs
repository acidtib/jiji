//! Observe-only local container discovery: the agent watches this project's jiji-labeled
//! containers on this host and records what it sees, without starting, stopping, or otherwise
//! managing them (Phase 2 scope; reconciliation/repair is Phase 6). Runs the engine binary
//! directly with an explicit argv (never a shell string), avoiding any quoting/injection concern
//! from a project name containing shell metacharacters.
//!
//! Field extraction mirrors `jiji-cli`'s `container_ops.rs::label_template`: Docker's `ps
//! --format` exposes a per-key `.Label "key"` function, but Podman's reporter has no such method
//! and needs `index .Labels "key"` instead -- confirmed live against both engines.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tracing::{debug, warn};

use crate::engine::Engine;
use crate::store::{AgentStore, Observation};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveryOutcome {
    Observed(Vec<Observation>),
    /// The engine binary isn't installed/reachable yet; a soft, retryable condition rather than a
    /// fatal one (`ensure_engine` on `jiji server setup` may simply not have run against this
    /// host yet).
    EngineUnavailable(String),
    /// The engine ran but reported an error (e.g. a broken daemon/socket).
    EngineError(String),
}

pub(crate) fn label_template(engine: Engine, key: &str) -> String {
    match engine {
        Engine::Docker => format!("{{{{.Label \"{key}\"}}}}"),
        Engine::Podman => format!("{{{{index .Labels \"{key}\"}}}}"),
    }
}

pub async fn discover(engine: Engine, project: &str) -> DiscoveryOutcome {
    discover_with_binary(engine.as_str(), engine, project).await
}

pub async fn discover_with_binary(binary: &str, engine: Engine, project: &str) -> DiscoveryOutcome {
    let template = format!(
        "{{{{.ID}}}}|{{{{.Names}}}}|{{{{.Image}}}}|{}|{}|{}|{}|{}|{}|{{{{.State}}}}",
        label_template(engine, "jiji.service"),
        label_template(engine, "jiji.catalog-managed"),
        label_template(engine, "jiji.replica"),
        label_template(engine, "jiji.deployment"),
        label_template(engine, "jiji.lease"),
        label_template(engine, "jiji.lifecycle"),
    );
    let output = tokio::process::Command::new(binary)
        .arg("ps")
        .arg("-a")
        .arg("--filter")
        .arg("label=jiji.managed=true")
        .arg("--filter")
        .arg(format!("label=jiji.project={project}"))
        .arg("--format")
        .arg(&template)
        .output()
        .await;

    match output {
        Ok(output) if output.status.success() => {
            DiscoveryOutcome::Observed(parse_lines(&String::from_utf8_lossy(&output.stdout)))
        }
        Ok(output) => DiscoveryOutcome::EngineError(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ),
        Err(error) => DiscoveryOutcome::EngineUnavailable(error.to_string()),
    }
}

fn parse_lines(stdout: &str) -> Vec<Observation> {
    stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(parse_line)
        .collect()
}

fn parse_line(line: &str) -> Option<Observation> {
    let mut fields = line.split('|');
    let container_id = fields.next()?.trim();
    if container_id.is_empty() {
        return None;
    }
    let name = fields.next().unwrap_or_default().trim().to_string();
    let image = fields.next().unwrap_or_default().trim().to_string();
    let service = fields.next().unwrap_or_default().trim().to_string();
    let catalog_managed = fields.next().unwrap_or_default().trim().to_string();
    let replica = fields.next().unwrap_or_default().trim().to_string();
    let deployment = fields.next().unwrap_or_default().trim().to_string();
    let lease = fields.next().unwrap_or_default().trim().to_string();
    let lifecycle = fields.next().unwrap_or_default().trim().to_string();
    let state = fields.next().unwrap_or_default().trim().to_string();
    let labels_json = serde_json::json!({
        "jiji.service": service,
        "jiji.catalog-managed": catalog_managed,
        "jiji.replica": replica,
        "jiji.deployment": deployment,
        "jiji.lease": lease,
        "jiji.lifecycle": lifecycle,
    })
    .to_string();
    Some(Observation {
        container_id: container_id.to_string(),
        name,
        image,
        labels_json,
        state,
    })
}

/// Reconstructs durable local leases from positive container-label evidence. A conflicting label
/// never steals an address: `recover_address_lease` returns false and the allocator continues to
/// reserve the existing claim until an operator resolves the conflict.
pub fn recover_labeled_leases(
    store: &AgentStore,
    observations: &[Observation],
) -> Result<usize, crate::store::StoreError> {
    let mut recovered = 0;
    for observation in observations {
        let Ok(labels) = serde_json::from_str::<serde_json::Value>(&observation.labels_json) else {
            continue;
        };
        let value = |key: &str| {
            labels
                .get(key)
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
        };
        if value("jiji.catalog-managed") != Some("true") {
            continue;
        }
        let (Some(deployment_id), Some(replica_id), Some(address)) = (
            value("jiji.deployment"),
            value("jiji.replica"),
            value("jiji.lease"),
        ) else {
            continue;
        };
        let Ok(address) = address.parse() else {
            continue;
        };
        if store.recover_address_lease(deployment_id, replica_id, address)? {
            recovered += 1;
        } else {
            warn!(
                %deployment_id,
                %replica_id,
                %address,
                "container label conflicts with a durable address lease; refusing to steal it"
            );
        }
    }
    Ok(recovered)
}

pub async fn reconcile_once(store: &AgentStore, engine: Engine, project: &str) -> DiscoveryOutcome {
    let outcome = discover(engine, project).await;
    let DiscoveryOutcome::Observed(observations) = &outcome else {
        return outcome;
    };
    if let Err(error) = recover_labeled_leases(store, observations) {
        warn!(%error, "failed to reconstruct address leases from container labels");
    }
    outcome
}

/// Ticks forever at `interval`, merging each pass's results into `store`. Never returns; intended
/// to be spawned as its own task. A malformed or engine-error pass leaves the previous
/// observations untouched (temporary absence never means deletion, matching the plan's invariant
/// even at this local, non-replicated layer) -- only a successful pass may prune stale entries.
pub async fn run_loop(
    store: Arc<Mutex<AgentStore>>,
    engine: Engine,
    project: String,
    interval: Duration,
) {
    let mut ticker = tokio::time::interval(interval);
    loop {
        ticker.tick().await;
        let outcome = discover(engine, &project).await;
        if let DiscoveryOutcome::Observed(observations) = &outcome {
            match store.lock() {
                Ok(store) => {
                    if let Err(error) = recover_labeled_leases(&store, observations) {
                        warn!(%error, "failed to reconstruct address leases from container labels");
                    }
                }
                Err(_) => warn!("local store lock poisoned; skipping lease recovery"),
            }
        }
        match outcome {
            DiscoveryOutcome::Observed(observations) => {
                let ids: Vec<String> = observations
                    .iter()
                    .map(|observation| observation.container_id.clone())
                    .collect();
                let store = match store.lock() {
                    Ok(store) => store,
                    Err(_) => {
                        warn!("local store lock poisoned; skipping this discovery pass");
                        continue;
                    }
                };
                for observation in &observations {
                    if let Err(error) = store.upsert_observation(observation) {
                        warn!(%error, "failed to record container observation");
                    }
                }
                if let Err(error) = store.retain_observations(&ids) {
                    warn!(%error, "failed to prune stale container observations");
                }
                let timestamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
                    .to_string();
                if let Err(error) = store.set_checkpoint("last_discovery_at", &timestamp) {
                    warn!(%error, "failed to record discovery checkpoint");
                }
            }
            DiscoveryOutcome::EngineUnavailable(detail) => {
                debug!(%detail, "container engine not reachable yet; will retry");
            }
            DiscoveryOutcome::EngineError(detail) => {
                warn!(%detail, "container discovery command failed; will retry");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    fn write_fake_engine(dir: &std::path::Path, script: &str) -> std::path::PathBuf {
        let path = dir.join("fake-engine");
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    }

    #[test]
    fn docker_and_podman_use_different_label_templates() {
        assert_eq!(
            label_template(Engine::Docker, "jiji.service"),
            "{{.Label \"jiji.service\"}}"
        );
        assert_eq!(
            label_template(Engine::Podman, "jiji.service"),
            "{{index .Labels \"jiji.service\"}}"
        );
    }

    #[test]
    fn parses_pipe_delimited_ps_output() {
        let observations = parse_lines(
            "abc123|demo-web-a|nginx:alpine|web|true|replica-1|deploy-1|10.0.0.4|active|running\n\n",
        );
        assert_eq!(
            observations,
            vec![Observation {
                container_id: "abc123".into(),
                name: "demo-web-a".into(),
                image: "nginx:alpine".into(),
                labels_json: serde_json::json!({
                    "jiji.service": "web",
                    "jiji.catalog-managed": "true",
                    "jiji.replica": "replica-1",
                    "jiji.deployment": "deploy-1",
                    "jiji.lease": "10.0.0.4",
                    "jiji.lifecycle": "active",
                })
                .to_string(),
                state: "running".into(),
            }]
        );
    }

    #[test]
    fn blank_and_malformed_lines_are_skipped_without_failing_the_pass() {
        assert!(parse_lines("\n   \n").is_empty());
        assert!(parse_lines("|missing-id|nginx:alpine|web||||||running").is_empty());
    }

    #[tokio::test]
    async fn observed_containers_are_parsed_from_a_successful_engine_run() {
        let dir = tempdir().unwrap();
        let engine_path = write_fake_engine(
            dir.path(),
            "echo 'abc123|demo-web-a|nginx:alpine|web|true|replica-1|deploy-1|10.0.0.4|active|running'",
        );
        let outcome =
            discover_with_binary(engine_path.to_str().unwrap(), Engine::Docker, "demo").await;
        assert_eq!(
            outcome,
            DiscoveryOutcome::Observed(vec![Observation {
                container_id: "abc123".into(),
                name: "demo-web-a".into(),
                image: "nginx:alpine".into(),
                labels_json: serde_json::json!({
                    "jiji.service": "web",
                    "jiji.catalog-managed": "true",
                    "jiji.replica": "replica-1",
                    "jiji.deployment": "deploy-1",
                    "jiji.lease": "10.0.0.4",
                    "jiji.lifecycle": "active",
                })
                .to_string(),
                state: "running".into(),
            }])
        );
    }

    #[tokio::test]
    async fn missing_engine_binary_is_reported_as_unavailable_not_fatal() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        let outcome = discover_with_binary(missing.to_str().unwrap(), Engine::Docker, "demo").await;
        assert!(matches!(outcome, DiscoveryOutcome::EngineUnavailable(_)));
    }

    #[test]
    fn labeled_container_recovers_and_reactivates_its_exact_lease() {
        let dir = tempdir().unwrap();
        let store = AgentStore::open(&dir.path().join("agent.sqlite3")).unwrap();
        store
            .claim_address_lease("deploy-1", "replica-1", "10.0.0.4".parse().unwrap())
            .unwrap();
        store.quarantine_address_lease("deploy-1", 999).unwrap();
        let observations =
            parse_lines("abc|demo-web|nginx|web|true|replica-1|deploy-1|10.0.0.4|active|running");
        assert_eq!(recover_labeled_leases(&store, &observations).unwrap(), 1);
        let lease = store.address_lease("deploy-1").unwrap().unwrap();
        assert_eq!(lease.state, "active");
        assert_eq!(lease.quarantine_until, None);

        let conflicting =
            parse_lines("def|demo-web-2|nginx|web|true|replica-2|deploy-2|10.0.0.4|active|running");
        assert_eq!(recover_labeled_leases(&store, &conflicting).unwrap(), 0);
        assert!(store.address_lease("deploy-2").unwrap().is_none());
    }

    #[tokio::test]
    async fn a_failing_engine_command_is_reported_as_an_engine_error() {
        let dir = tempdir().unwrap();
        let engine_path = write_fake_engine(dir.path(), "echo 'boom' >&2; exit 1");
        let outcome =
            discover_with_binary(engine_path.to_str().unwrap(), Engine::Docker, "demo").await;
        assert_eq!(outcome, DiscoveryOutcome::EngineError("boom".to_string()));
    }
}
