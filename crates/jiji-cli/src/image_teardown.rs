use std::collections::BTreeSet;

use jiji_config::{Config, ContainerEngine};
use jiji_ssh::SshSession;

use crate::container_ops;

pub enum ImageOutcome {
    Removed,
    NotPresent,
    RetainedInUseBy(Vec<String>),
}

/// Distinct `image:` references configured for the project's services. Build-produced tags are
/// intentionally excluded until retained-image pruning can identify them safely. Never use a glob
/// against image names here.
pub fn compute_candidates(config: &Config) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut candidates = Vec::new();
    for service in config.services.values() {
        if let Some(image) = &service.image {
            if seen.insert(image.clone()) {
                candidates.push(image.clone());
            }
        }
    }
    candidates
}

/// Removes each candidate image only if it exists and nothing else on the host still runs from
/// it (checked after this project's own containers have already been removed by the caller, so
/// "referenced elsewhere" only ever means a genuinely unrelated container).
pub async fn discover_and_remove(
    session: &SshSession,
    engine: ContainerEngine,
    images: &[String],
) -> anyhow::Result<Vec<(String, ImageOutcome)>> {
    let mut results = Vec::with_capacity(images.len());
    for image in images {
        if !container_ops::image_exists(session, engine, image).await? {
            results.push((image.clone(), ImageOutcome::NotPresent));
            continue;
        }
        let referenced = container_ops::image_referenced_elsewhere(session, engine, image).await?;
        if !referenced.is_empty() {
            results.push((image.clone(), ImageOutcome::RetainedInUseBy(referenced)));
            continue;
        }
        container_ops::remove_image_if_present(session, engine, image).await?;
        results.push((image.clone(), ImageOutcome::Removed));
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
    fn distinct_images_are_collected_across_services() {
        let config = config(
            "project: demo\nbuilder: { engine: docker }\nservers: {}\nservices:\n  web:\n    image: example/web:1\n  worker:\n    image: example/worker:1\n",
        );
        let mut candidates = compute_candidates(&config);
        candidates.sort();
        assert_eq!(
            candidates,
            vec!["example/web:1".to_string(), "example/worker:1".to_string()]
        );
    }

    #[test]
    fn shared_image_across_services_deduplicates() {
        let config = config(
            "project: demo\nbuilder: { engine: docker }\nservers: {}\nservices:\n  web:\n    image: example/shared:1\n  worker:\n    image: example/shared:1\n",
        );
        assert_eq!(
            compute_candidates(&config),
            vec!["example/shared:1".to_string()]
        );
    }

    #[test]
    fn services_without_an_image_are_skipped() {
        let config = config(
            "project: demo\nbuilder: { engine: docker }\nservers: {}\nservices:\n  web:\n    build: { context: . }\n",
        );
        assert!(compute_candidates(&config).is_empty());
    }
}
