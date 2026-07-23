use jiji_config::{Config, ContainerEngine};
use jiji_ssh::SshSession;

use crate::{
    container_ops, env_resolution, image_teardown, network_teardown, proxy_teardown,
    volume_teardown,
};

pub struct ServerTeardownPlan {
    pub server_name: String,
    pub containers: Vec<container_ops::ContainerSummary>,
    /// Proxy route candidates that are actually present on this host's kamal-proxy.
    pub proxy_routes: Vec<String>,
    pub volumes: Vec<volume_teardown::DiscoveredVolume>,
    /// Image candidates that actually exist on this host.
    pub images: Vec<String>,
    pub network: network_teardown::NetworkTeardownStatus,
    /// Whether `env_resolution::project_staging_dir` exists on this host (staged env files and
    /// uploaded mount content from past deploys).
    pub project_directory_exists: bool,
    /// Non-empty means this host is skipped entirely: nothing destructive is attempted.
    pub blockers: Vec<String>,
}

/// Fully read-only: label-filtered container listing, existence checks for volume/image/route
/// candidates, and reading the installed network generation. No destructive operation happens
/// here.
pub async fn discover(
    session: &SshSession,
    engine: ContainerEngine,
    config: &Config,
    project: &str,
    server_name: &str,
    include_volumes: bool,
) -> anyhow::Result<ServerTeardownPlan> {
    let containers = container_ops::list_managed_containers(session, engine, project).await?;

    let route_candidates = proxy_teardown::compute_route_candidates(config, project);
    let existing_routes = proxy_teardown::list_routes(session, engine).await?;
    let proxy_routes: Vec<String> = route_candidates
        .into_iter()
        .filter(|route| existing_routes.contains(route))
        .collect();

    let volumes = if include_volumes {
        let candidates = volume_teardown::compute_candidates(config);
        volume_teardown::discover(session, engine, &candidates, project).await?
    } else {
        Vec::new()
    };

    let image_candidates = image_teardown::compute_candidates(config);
    let mut images = Vec::with_capacity(image_candidates.len());
    for image in &image_candidates {
        if container_ops::image_exists(session, engine, image).await? {
            images.push(image.clone());
        }
    }

    let network = network_teardown::discover(session, engine, project).await?;
    let blockers = render_blockers(&network.other_project_containers);
    let project_directory_exists = project_directory_exists(session, project).await?;

    Ok(ServerTeardownPlan {
        server_name: server_name.to_string(),
        containers,
        proxy_routes,
        volumes,
        images,
        network,
        project_directory_exists,
        blockers,
    })
}

async fn project_directory_exists(session: &SshSession, project: &str) -> anyhow::Result<bool> {
    let command = format!("test -d {}", env_resolution::project_staging_dir(project));
    Ok(session.execute(&command).await?.success)
}

fn render_blockers(other_project_containers: &[container_ops::ContainerSummary]) -> Vec<String> {
    other_project_containers
        .iter()
        .map(|container| {
            format!(
                "another jiji project's container '{}' (project '{}') is still present on this host; tear that project down first, or remove it manually",
                container.name,
                container.project.as_deref().unwrap_or("unlabeled")
            )
        })
        .collect()
}

pub fn has_blockers(plan: &ServerTeardownPlan) -> bool {
    !plan.blockers.is_empty()
}

pub fn render_summary(plan: &ServerTeardownPlan) -> String {
    let mut parts = vec![format!("{} container(s)", plan.containers.len())];
    if !plan.volumes.is_empty() {
        let removable = plan
            .volumes
            .iter()
            .filter(|volume| volume.exists && volume.blocked_by.is_none())
            .count();
        parts.push(format!("{removable} volume(s)"));
    }
    if !plan.images.is_empty() {
        parts.push(format!("{} image(s)", plan.images.len()));
    }
    if !plan.proxy_routes.is_empty() {
        parts.push(format!("{} proxy route(s)", plan.proxy_routes.len()));
    }
    if plan.network.installed_generation.is_some() {
        parts.push("private network".to_string());
    }
    if plan.project_directory_exists {
        parts.push("staged files/env".to_string());
    }
    format!("{}: {}", plan.server_name, parts.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use container_ops::ContainerSummary;

    fn container(name: &str, project: Option<&str>) -> ContainerSummary {
        ContainerSummary {
            name: name.to_string(),
            project: project.map(str::to_string),
            service: None,
            server: None,
            status: "running".to_string(),
        }
    }

    #[test]
    fn other_project_container_is_a_blocker() {
        let blockers = render_blockers(&[container("other-web-a", Some("other"))]);
        assert_eq!(blockers.len(), 1);
        assert!(blockers[0].contains("other"));
    }

    #[test]
    fn render_blockers_trusts_its_input_and_never_panics_on_an_unlabeled_container() {
        // render_blockers does no filtering of its own -- excluding kamal-proxy (jiji.managed=true
        // but no jiji.project label) from ever reaching here is
        // container_ops::list_other_project_containers's job (see its own test:
        // list_other_project_containers_excludes_the_named_project_and_unlabeled_containers,
        // confirmed live). This only guards that an unlabeled entry, if it ever did arrive, would
        // render descriptively rather than panicking.
        let blockers = render_blockers(&[container("kamal-proxy", None)]);
        assert_eq!(blockers.len(), 1);
        assert!(blockers[0].contains("unlabeled"));
    }

    #[test]
    fn no_other_project_containers_means_no_blockers() {
        assert!(render_blockers(&[]).is_empty());
    }

    fn empty_plan(server_name: &str) -> ServerTeardownPlan {
        ServerTeardownPlan {
            server_name: server_name.to_string(),
            containers: Vec::new(),
            proxy_routes: Vec::new(),
            volumes: Vec::new(),
            images: Vec::new(),
            network: network_teardown::NetworkTeardownStatus {
                installed_generation: None,
                other_project_containers: Vec::new(),
            },
            project_directory_exists: false,
            blockers: Vec::new(),
        }
    }

    #[test]
    fn summary_always_reports_container_count() {
        let plan = empty_plan("app");
        assert_eq!(render_summary(&plan), "app: 0 container(s)");
    }

    #[test]
    fn summary_includes_staged_files_only_when_the_project_directory_exists() {
        let mut plan = empty_plan("app");
        assert!(!render_summary(&plan).contains("staged files/env"));
        plan.project_directory_exists = true;
        assert!(render_summary(&plan).contains("staged files/env"));
    }

    #[test]
    fn summary_includes_private_network_only_when_installed() {
        let mut plan = empty_plan("app");
        plan.network.installed_generation = Some("abc123".to_string());
        assert!(render_summary(&plan).contains("private network"));
    }

    #[test]
    fn has_blockers_reflects_blocker_list() {
        let mut plan = empty_plan("app");
        assert!(!has_blockers(&plan));
        plan.blockers.push("blocked".to_string());
        assert!(has_blockers(&plan));
    }
}
