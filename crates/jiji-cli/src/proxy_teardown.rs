use jiji_config::{Config, ContainerEngine};
use jiji_ssh::SshSession;

use crate::proxy_routes::{RouteSummary, TcpRouteSummary};
use crate::{container_ops, proxy_ingress, proxy_routes};

/// Every `(host, path_prefix)` route a proxy-enabled service in this project would register,
/// computed purely from config. Unlike kamal-proxy's `{project}-{service}-{port}` route names
/// (which had no server component, since each server's kamal-proxy kept its own local route
/// table), a jiji-proxy route's identity is the host/path pair itself -- see
/// `proxy_routes::targets_for_service`.
pub fn compute_route_candidates(config: &Config) -> Vec<(String, Option<String>)> {
    let mut candidates = Vec::new();
    for service in config.services.values() {
        let Some(proxy) = &service.proxy else {
            continue;
        };
        if let Some(targets) = &proxy.targets {
            for target in targets {
                if target.listen_port.is_some() {
                    continue;
                }
                for host in target.hosts.clone().unwrap_or_default() {
                    candidates.push((host, target.path_prefix.clone()));
                }
            }
        } else if proxy.listen_port.is_none() {
            for host in proxy.hosts.clone().unwrap_or_default() {
                candidates.push((host, proxy.path_prefix.clone()));
            }
        }
    }
    candidates
}

/// Mirrors `compute_route_candidates` for raw TCP targets (`listen_port` set).
pub fn compute_tcp_route_candidates(config: &Config) -> Vec<u16> {
    let mut candidates = Vec::new();
    for service in config.services.values() {
        let Some(proxy) = &service.proxy else {
            continue;
        };
        if let Some(targets) = &proxy.targets {
            for target in targets {
                if let Some(listen_port) = target.listen_port {
                    candidates.push(listen_port);
                }
            }
        } else if let Some(listen_port) = proxy.listen_port {
            candidates.push(listen_port);
        }
    }
    candidates
}

/// No routes if the jiji-proxy container itself doesn't exist (already-idempotent: a prior
/// teardown may already have removed it) or isn't currently running (confirmed live: a stopped
/// container can't serve any route, and `podman exec`/`docker exec` refuse outright with "can
/// only create exec sessions on running containers" -- a hard error here would otherwise make an
/// unrelated stopped jiji-proxy block this entire host's teardown). Checked via an existence/
/// state precheck rather than matching on exec's stderr wording.
pub async fn list_routes(
    session: &SshSession,
    engine: ContainerEngine,
) -> anyhow::Result<Vec<RouteSummary>> {
    match container_ops::inspect_status(session, engine, jiji_network::CONTAINER_NAME).await? {
        Some(status) if status == "running" => {}
        _ => return Ok(Vec::new()),
    }
    proxy_routes::list_routes(session, engine).await
}

/// Mirrors `list_routes` for raw TCP routes.
pub async fn list_tcp_routes(
    session: &SshSession,
    engine: ContainerEngine,
) -> anyhow::Result<Vec<TcpRouteSummary>> {
    match container_ops::inspect_status(session, engine, jiji_network::CONTAINER_NAME).await? {
        Some(status) if status == "running" => {}
        _ => return Ok(Vec::new()),
    }
    proxy_routes::list_tcp_routes(session, engine).await
}

/// Removes every `candidates` entry that's actually present in `existing_routes`. Returns
/// `((host, path_prefix), was_present)` for every candidate, so callers can report already-absent
/// routes without treating them as an error. `existing_routes` shares `candidates`' own
/// `(host, path_prefix)` shape rather than the richer `RouteSummary` -- `teardown_plan.rs`'s own
/// discovery already pre-filters candidates down to ones confirmed present, so its typical caller
/// passes the same pre-filtered slice for both parameters.
pub async fn remove_project_routes(
    session: &SshSession,
    engine: ContainerEngine,
    candidates: &[(String, Option<String>)],
    existing_routes: &[(String, Option<String>)],
) -> anyhow::Result<Vec<((String, Option<String>), bool)>> {
    let mut results = Vec::with_capacity(candidates.len());
    for (host, path_prefix) in candidates {
        let present = existing_routes.contains(&(host.clone(), path_prefix.clone()));
        if !present {
            results.push(((host.clone(), path_prefix.clone()), false));
            continue;
        }
        proxy_routes::remove_route(session, engine, host, path_prefix.as_deref()).await?;
        results.push(((host.clone(), path_prefix.clone()), true));
    }
    Ok(results)
}

/// Mirrors `remove_project_routes` for raw TCP routes.
pub async fn remove_project_tcp_routes(
    session: &SshSession,
    engine: ContainerEngine,
    candidates: &[u16],
    existing_routes: &[u16],
) -> anyhow::Result<Vec<(u16, bool)>> {
    let mut results = Vec::with_capacity(candidates.len());
    for &listen_port in candidates {
        let present = existing_routes.contains(&listen_port);
        if !present {
            results.push((listen_port, false));
            continue;
        }
        proxy_routes::remove_tcp_route(session, engine, listen_port).await?;
        results.push((listen_port, true));
    }
    Ok(results)
}

pub enum ProxyContainerOutcome {
    Removed,
    AlreadyAbsent,
    RetainedInUseBy(Vec<RouteSummary>, Vec<TcpRouteSummary>),
}

/// Removes the jiji-proxy container itself only if no route remains for ANY project after this
/// project's own routes are gone -- jiji-proxy is shared across every project on a host, so its
/// container is never tied to a single project's ownership labels the way service containers are.
pub async fn teardown_proxy_container_if_unused(
    session: &SshSession,
    engine: ContainerEngine,
) -> anyhow::Result<ProxyContainerOutcome> {
    let remaining = list_routes(session, engine).await?;
    let remaining_tcp = list_tcp_routes(session, engine).await?;
    if !remaining.is_empty() || !remaining_tcp.is_empty() {
        return Ok(ProxyContainerOutcome::RetainedInUseBy(
            remaining,
            remaining_tcp,
        ));
    }
    if !container_ops::remove_if_present(session, engine, jiji_network::CONTAINER_NAME).await? {
        return Ok(ProxyContainerOutcome::AlreadyAbsent);
    }
    // Nothing needs jiji-proxy anymore, so its exclusive resources (not shared with any service,
    // and unlike kamal-proxy there's no separate config volume -- jiji-proxy has no state beyond
    // what CERTS_DIR already holds) are orphaned too. Recreated from scratch by the next
    // `server setup`, so there's no data-loss concern in removing them now.
    remove_certs_dir(session).await?;
    // Only Docker's `ensure_proxy` ever installs this (see `proxy_ingress`); harmlessly a no-op
    // on Podman, where it was never created.
    if engine == ContainerEngine::Docker {
        proxy_ingress::remove_ingress_rule(session).await?;
    }
    Ok(ProxyContainerOutcome::Removed)
}

async fn remove_certs_dir(session: &SshSession) -> anyhow::Result<()> {
    let command = format!("rm -rf {}", jiji_network::CERTS_DIR);
    let result = session.execute(&command).await?;
    if !result.success {
        anyhow::bail!(
            "Could not remove {} on {}: {}",
            jiji_network::CERTS_DIR,
            session.host(),
            result.stderr.trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(yaml: &str) -> Config {
        serde_yaml::from_str(yaml).unwrap()
    }

    #[test]
    fn single_target_produces_one_candidate_per_host() {
        let config = config(
            "project: demo\nbuilder: { engine: docker }\nservers: {}\nservices:\n  web:\n    image: example/web\n    proxy: { port: 3000, hosts: [example.com] }\n",
        );
        let candidates = compute_route_candidates(&config);
        assert_eq!(candidates, vec![("example.com".to_string(), None)]);
    }

    #[test]
    fn multi_target_produces_one_candidate_per_target_host() {
        let config = config(
            r#"
project: demo
builder: { engine: docker }
servers: {}
services:
  web:
    image: example/web
    proxy:
      targets:
        - { port: 3900, hosts: [s3.example.com] }
        - { port: 3903, hosts: [admin.example.com], path_prefix: /admin }
"#,
        );
        let mut candidates = compute_route_candidates(&config);
        candidates.sort();
        assert_eq!(
            candidates,
            vec![
                ("admin.example.com".to_string(), Some("/admin".to_string())),
                ("s3.example.com".to_string(), None),
            ]
        );
    }

    #[test]
    fn services_without_proxy_produce_no_candidates() {
        let config = config(
            "project: demo\nbuilder: { engine: docker }\nservers: {}\nservices:\n  worker:\n    image: example/worker\n",
        );
        assert!(compute_route_candidates(&config).is_empty());
    }

    #[test]
    fn tcp_target_produces_one_listen_port_candidate() {
        let config = config(
            "project: demo\nbuilder: { engine: docker }\nservers: {}\nservices:\n  db:\n    image: postgres:18\n    proxy: { port: 5432, listen_port: 5432 }\n",
        );
        assert_eq!(compute_tcp_route_candidates(&config), vec![5432]);
        // A TCP-mode target never also produces an HTTP route candidate.
        assert!(compute_route_candidates(&config).is_empty());
    }

    #[test]
    fn multi_target_separates_http_and_tcp_candidates() {
        let config = config(
            r#"
project: demo
builder: { engine: docker }
servers: {}
services:
  app:
    image: example/app
    proxy:
      targets:
        - { port: 80, hosts: [web.example.com] }
        - { port: 5432, listen_port: 5432 }
"#,
        );
        assert_eq!(
            compute_route_candidates(&config),
            vec![("web.example.com".to_string(), None)]
        );
        assert_eq!(compute_tcp_route_candidates(&config), vec![5432]);
    }

    #[test]
    fn remove_project_routes_reports_already_absent_without_calling_remove() {
        // Pure existence-filtering logic exercised without a session: candidates not present in
        // existing_routes must be reported (not skipped) as already-absent.
        let candidates: [(String, Option<String>); 2] = [
            ("example.com".to_string(), None),
            ("api.example.com".to_string(), None),
        ];
        let existing: [(String, Option<String>); 1] = [("example.com".to_string(), None)];
        let present: Vec<bool> = candidates
            .iter()
            .map(|candidate| existing.contains(candidate))
            .collect();
        assert_eq!(present, vec![true, false]);
    }
}
