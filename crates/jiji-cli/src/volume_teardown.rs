use std::collections::BTreeSet;

use jiji_config::{Config, ContainerEngine};
use jiji_ssh::SshSession;

use crate::container_ops;
use crate::container_runtime::is_named_volume_source;

pub struct VolumeCandidate {
    pub name: String,
    pub service: String,
}

pub struct DiscoveredVolume {
    pub name: String,
    pub service: String,
    pub exists: bool,
    /// `Some(description)` if a live attacher makes deletion ambiguous: either a different
    /// project's container, or a container with no `jiji.project` label at all. `None` means
    /// either nothing is attached, or every attacher belongs to this same project.
    pub blocked_by: Option<String>,
}

/// Named-volume candidates computed from config, mirroring `container_runtime::render_volumes`'s
/// exact naming rule (`{service}-{source}`) so this can never drift from what deploy actually
/// creates. Known, accepted gap: a volume belonging to a service already removed from config is
/// invisible here, since there's nothing left to compute its name from.
pub fn compute_candidates(config: &Config) -> Vec<VolumeCandidate> {
    let mut seen = BTreeSet::new();
    let mut candidates = Vec::new();
    for service_name in config.services.keys() {
        for candidate in compute_candidates_for_service(config, service_name) {
            if seen.insert(candidate.name.clone()) {
                candidates.push(candidate);
            }
        }
    }
    candidates
}

/// Same naming rule as `compute_candidates`, scoped to a single service -- used by `jiji service
/// remove --volumes` so a partial (`-S`-filtered) removal never touches another service's volumes.
pub fn compute_candidates_for_service(config: &Config, service_name: &str) -> Vec<VolumeCandidate> {
    let mut seen = BTreeSet::new();
    let mut candidates = Vec::new();
    let Some(service) = config.services.get(service_name) else {
        return candidates;
    };
    for volume in &service.volumes {
        let Some(colon) = volume.find(':') else {
            continue;
        };
        let source = &volume[..colon];
        if !is_named_volume_source(source) {
            continue;
        }
        let name = format!("{service_name}-{source}");
        if seen.insert(name.clone()) {
            candidates.push(VolumeCandidate {
                name,
                service: service_name.to_string(),
            });
        }
    }
    candidates
}

pub async fn discover(
    session: &SshSession,
    engine: ContainerEngine,
    candidates: &[VolumeCandidate],
    project: &str,
) -> anyhow::Result<Vec<DiscoveredVolume>> {
    let mut discovered = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let exists = container_ops::volume_exists(session, engine, &candidate.name).await?;
        let blocked_by = if exists {
            let attached =
                container_ops::volume_attached_projects(session, engine, &candidate.name).await?;
            attached
                .into_iter()
                .find_map(|attacher| describe_blocking_attacher(attacher, project))
        } else {
            None
        };
        discovered.push(DiscoveredVolume {
            name: candidate.name.clone(),
            service: candidate.service.clone(),
            exists,
            blocked_by,
        });
    }
    Ok(discovered)
}

fn describe_blocking_attacher(attacher: Option<String>, this_project: &str) -> Option<String> {
    match attacher {
        Some(project) if project == this_project => None,
        Some(other_project) => Some(other_project),
        None => Some("an unlabeled container".to_string()),
    }
}

/// Removes every discovered volume that exists and has no blocking attacher. Returns
/// `(volume_name, was_removed)` for every entry, so already-absent or blocked volumes are
/// reported rather than silently skipped.
pub async fn remove(
    session: &SshSession,
    engine: ContainerEngine,
    volumes: &[DiscoveredVolume],
) -> anyhow::Result<Vec<(String, bool)>> {
    let mut results = Vec::with_capacity(volumes.len());
    for volume in volumes {
        if !volume.exists || volume.blocked_by.is_some() {
            results.push((volume.name.clone(), false));
            continue;
        }
        let removed =
            container_ops::remove_volume_if_present(session, engine, &volume.name).await?;
        results.push((volume.name.clone(), removed));
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(yaml: &str) -> Config {
        serde_yaml::from_str(yaml).unwrap()
    }

    #[test]
    fn named_volume_candidate_matches_deploy_naming() {
        let config = config(
            "project: demo\nbuilder: { engine: docker }\nservers: {}\nservices:\n  web:\n    image: example/web\n    volumes: [\"web_storage:/data\"]\n",
        );
        let candidates = compute_candidates(&config);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].name, "web-web_storage");
        assert_eq!(candidates[0].service, "web");
    }

    #[test]
    fn bind_mounts_are_never_candidates() {
        let config = config(
            "project: demo\nbuilder: { engine: docker }\nservers: {}\nservices:\n  web:\n    image: example/web\n    volumes: [\"/data:/data\", \"./relative:/data\"]\n",
        );
        assert!(compute_candidates(&config).is_empty());
    }

    #[test]
    fn duplicate_volume_declarations_produce_one_candidate() {
        let config = config(
            "project: demo\nbuilder: { engine: docker }\nservers: {}\nservices:\n  web:\n    image: example/web\n    volumes: [\"storage:/a\", \"storage:/b\"]\n",
        );
        assert_eq!(compute_candidates(&config).len(), 1);
    }

    #[test]
    fn per_service_candidates_never_include_a_sibling_services_volumes() {
        let config = config(
            "project: demo\nbuilder: { engine: docker }\nservers: {}\nservices:\n  web:\n    image: example/web\n    volumes: [\"web_storage:/data\"]\n  worker:\n    image: example/worker\n    volumes: [\"worker_storage:/data\"]\n",
        );
        let web_only = compute_candidates_for_service(&config, "web");
        assert_eq!(web_only.len(), 1);
        assert_eq!(web_only[0].name, "web-web_storage");

        assert!(compute_candidates_for_service(&config, "does-not-exist").is_empty());
    }

    #[test]
    fn same_project_attacher_is_not_blocking() {
        assert_eq!(
            describe_blocking_attacher(Some("demo".to_string()), "demo"),
            None
        );
    }

    #[test]
    fn different_project_attacher_blocks_with_its_name() {
        assert_eq!(
            describe_blocking_attacher(Some("other".to_string()), "demo"),
            Some("other".to_string())
        );
    }

    #[test]
    fn unlabeled_attacher_blocks_as_ambiguous() {
        assert_eq!(
            describe_blocking_attacher(None, "demo"),
            Some("an unlabeled container".to_string())
        );
    }
}
