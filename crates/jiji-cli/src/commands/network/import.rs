//! `jiji network import`: one-way seeding of catalog history from a stopped old installation
//! (Phase 8, "Clean Cutover, Optional Import, and Release"). Operator convenience, not a
//! compatibility layer: it only ever writes `Stopped` catalog rows so `jiji service`/`jiji
//! network catalog` show continuity instead of a blank slate, never marks anything `Active`,
//! never allocates an address lease, and never touches a replica whose existing catalog record
//! is already live (`Candidate`/`Active`/`Draining`) -- a normal `jiji deploy` remains the only
//! way to bring a service up on the new dynamic-lease runtime. Requires the target host's agent
//! to already be running (`jiji server setup`), since committing a catalog record needs the
//! agent's own signing identity; there is no offline commit path.
//!
//! Safely retryable: every commit's `deployment_id` is deterministic from the discovered
//! container's name, and the pre-commit catalog read skips any replica that is no longer eligible
//! (already live, or already imported with the same deployment ID), so re-running after an
//! interruption only ever fills in what's still missing.

use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::sync::Arc;

use jiji_agent::api::{RequestBody, ResponseBody};
use jiji_agent::catalog::{DeploymentState, HealthState};
use jiji_config::validate_config;
use jiji_network::{NetworkPlanner, ServerPlan};
use jiji_ssh::SshSession;
use jiji_tui::Ui;

use crate::{agent_client, container_ops, placement, ssh_adapter};

use super::backup::with_project_maintenance_lock;

#[derive(Debug, Clone)]
struct ImportCandidate {
    server: String,
    service: String,
    replica_id: String,
    deployment_id: String,
    address: Ipv4Addr,
    image: String,
}

pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    hosts: Option<&str>,
    dry_run: bool,
    yes: bool,
) -> anyhow::Result<()> {
    let start = std::env::current_dir()?;
    let (config, _path) = crate::config_loading::load_config_for_ssh(
        environment,
        config_file.map(std::path::Path::new),
        &start,
    )
    .await?;
    if !validate_config(&config).valid {
        anyhow::bail!("Configuration is invalid; fix it before importing");
    }
    let ssh = config
        .ssh
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No `ssh:` section is configured"))?;
    let plan = NetworkPlanner::new()
        .plan(&config)
        .map_err(|error| anyhow::anyhow!("Could not build the network plan: {error}"))?;
    let selected: Vec<ServerPlan> = plan
        .select_hosts(&split(hosts))?
        .into_iter()
        .cloned()
        .collect();
    if selected.is_empty() {
        anyhow::bail!("No servers are configured. Add a `servers:` entry and retry.");
    }
    let engine = config.builder.engine;

    let mut sessions: BTreeMap<String, Arc<SshSession>> = BTreeMap::new();
    let mut connect_failures = Vec::new();
    for server_plan in &selected {
        let named_server = config.servers.get(&server_plan.name).ok_or_else(|| {
            anyhow::anyhow!(
                "Server '{}' selected by the network plan is not configured",
                server_plan.name
            )
        })?;
        let options = ssh_adapter::connect_options(&server_plan.name, named_server, ssh)?;
        match SshSession::connect(&options).await {
            Ok(session) => {
                sessions.insert(server_plan.name.clone(), Arc::new(session));
            }
            Err(error) => connect_failures.push(format!("{}: {error}", server_plan.name)),
        }
    }
    if !connect_failures.is_empty() {
        for session in sessions.values() {
            session.close().await;
        }
        anyhow::bail!(
            "Could not connect to server(s): {}. Restore SSH access and retry.",
            connect_failures.join(", ")
        );
    }

    let candidates = match discover_candidates(&config, engine, &sessions).await {
        Ok(candidates) => candidates,
        Err(error) => {
            for session in sessions.values() {
                session.close().await;
            }
            return Err(error);
        }
    };

    if candidates.is_empty() {
        for session in sessions.values() {
            session.close().await;
        }
        Ui::say(
            "Nothing to import: no old containers found without an existing catalog record.",
            0,
        );
        return Ok(());
    }

    Ui::section("Import Plan:");
    for candidate in &candidates {
        Ui::say(
            &format!(
                "{}: {} -> replica {} deployment {} address {} image {}",
                candidate.server,
                candidate.service,
                candidate.replica_id,
                candidate.deployment_id,
                candidate.address,
                candidate.image,
            ),
            1,
        );
    }

    if dry_run {
        for session in sessions.values() {
            session.close().await;
        }
        return Ok(());
    }

    if !yes
        && !Ui::confirm(
            &format!(
                "Import {} replica(s) as historical (Stopped) catalog records? This never marks \
                 anything active and never touches a live replica.",
                candidates.len()
            ),
            false,
        )?
    {
        for session in sessions.values() {
            session.close().await;
        }
        anyhow::bail!("Import cancelled.");
    }

    let selected_for_lock = selected.clone();
    let project_for_lock = config.project.clone();
    let servers_for_lock = config.servers.clone();
    let ssh_for_lock = ssh.clone();
    let sessions_for_op = sessions.clone();
    let project = config.project.clone();
    let result = with_project_maintenance_lock(
        &project_for_lock,
        &servers_for_lock,
        &ssh_for_lock,
        &selected_for_lock,
        "jiji network import".to_string(),
        move || async move { commit_candidates(&sessions_for_op, &project, candidates).await },
    )
    .await;
    for session in sessions.values() {
        session.close().await;
    }
    result
}

async fn discover_candidates(
    config: &jiji_config::Config,
    engine: jiji_config::ContainerEngine,
    sessions: &BTreeMap<String, Arc<SshSession>>,
) -> anyhow::Result<Vec<ImportCandidate>> {
    let mut candidates = Vec::new();
    for server_plan_name in sessions.keys() {
        let session = &sessions[server_plan_name];
        let containers =
            container_ops::list_managed_containers(session, engine, &config.project).await?;
        let catalog = agent_client::catalog(session, &config.project)
            .await
            .map_err(|error| {
                anyhow::anyhow!(
                    "Agent not reachable on {server_plan_name}: {error}. Run `jiji server setup` first."
                )
            })?;
        for (service_name, service) in &config.services {
            if !service.servers.iter().any(|s| s == server_plan_name) {
                continue;
            }
            let Some(container) = containers.iter().find(|c| {
                c.service.as_deref() == Some(service_name.as_str())
                    && c.server.as_deref() == Some(server_plan_name.as_str())
            }) else {
                continue;
            };
            let replica_id = placement::endpoint_replica_id(
                &config.project,
                service_name,
                service,
                server_plan_name,
            )?;
            if let Some(existing) = catalog.iter().find(|r| r.replica_id == replica_id) {
                if !matches!(
                    existing.state,
                    DeploymentState::Stopped | DeploymentState::Tombstoned
                ) {
                    continue;
                }
            }
            let deployment_id = format!("imported-{}", container.name);
            if catalog
                .iter()
                .any(|record| record.deployment_id == deployment_id)
            {
                continue;
            }
            let Some(address) =
                container_ops::inspect_ip_address(session, engine, &container.name).await?
            else {
                Ui::result_warn(
                    server_plan_name,
                    &format!(
                        "{} has no discoverable address; skipping import, remove or migrate it manually",
                        container.name
                    ),
                );
                continue;
            };
            let image = container_ops::inspect_image(session, engine, &container.name)
                .await?
                .unwrap_or_else(|| "unknown".to_string());
            candidates.push(ImportCandidate {
                server: server_plan_name.clone(),
                service: service_name.clone(),
                replica_id,
                deployment_id,
                address,
                image,
            });
        }
    }
    Ok(candidates)
}

async fn commit_candidates(
    sessions: &BTreeMap<String, Arc<SshSession>>,
    project: &str,
    candidates: Vec<ImportCandidate>,
) -> anyhow::Result<()> {
    let mut failures = Vec::new();
    for candidate in &candidates {
        let session = &sessions[&candidate.server];
        let body = RequestBody::CatalogCommit {
            service: candidate.service.clone(),
            replica_id: candidate.replica_id.clone(),
            deployment_id: candidate.deployment_id.clone(),
            address: candidate.address.to_string(),
            ports: Vec::new(),
            image: candidate.image.clone(),
            state: DeploymentState::Stopped,
            health: HealthState::Unknown,
        };
        match agent_client::call(
            session,
            project,
            Some(candidate.deployment_id.clone()),
            body,
        )
        .await
        {
            Ok(ResponseBody::CatalogCommitted { .. }) => Ui::result_ok(
                &candidate.server,
                &format!("imported {} as {}", candidate.service, candidate.replica_id),
            ),
            Ok(other) => failures.push(format!(
                "{}: unexpected response {other:?}",
                candidate.server
            )),
            Err(error) => failures.push(format!("{}: {error}", candidate.server)),
        }
    }
    if !failures.is_empty() {
        anyhow::bail!(
            "Import failed for {} replica(s): {}",
            failures.len(),
            failures.join("; ")
        );
    }
    Ok(())
}

fn split(value: Option<&str>) -> Vec<String> {
    value
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}
