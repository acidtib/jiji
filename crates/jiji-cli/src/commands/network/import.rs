//! One-way seeding of catalog history from a stopped old installation, run as part of `jiji
//! server setup --import` (Phase 8, "Clean Cutover, Optional Import, and Release"). Operator
//! convenience, not a compatibility layer: it only ever writes `Stopped` catalog rows so `jiji
//! service`/`jiji network catalog` show continuity instead of a blank slate, never marks anything
//! `Active`, never allocates an address lease, and never touches a replica whose existing catalog
//! record is already live (`Candidate`/`Active`/`Draining`) -- a normal `jiji deploy` remains the
//! only way to bring a service up on the new dynamic-lease runtime. Requires the target host's
//! agent to already be running, since committing a catalog record needs the agent's own signing
//! identity; there is no offline commit path -- `run_import` is therefore only ever called after
//! `commands::server::setup::setup_agents` has succeeded.
//!
//! Safely retryable: every commit's `deployment_id` is deterministic from the discovered
//! container's name, and the pre-commit catalog read skips any replica that is no longer eligible
//! (already live, or already imported with the same deployment ID), so re-running after an
//! interruption only ever fills in what's still missing.
//!
//! There is no standalone `jiji network import` command and no dedicated lock: the caller
//! (`jiji server setup`) already holds a `HostRuntime` lock on every targeted server for the
//! whole run, which is sufficient for writing catalog records scoped to those same servers.

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::net::Ipv4Addr;
use std::sync::Arc;

use jiji_agent::api::{RequestBody, ResponseBody};
use jiji_agent::catalog::{DeploymentState, HealthState};
use jiji_agent::AgentPaths;
use jiji_network::NetworkPlan;
use jiji_ssh::SshSession;
use jiji_tui::Ui;

use crate::{agent_client, container_ops, placement};

use super::assess;

#[derive(Debug, Clone)]
struct ImportCandidate {
    server: String,
    service: String,
    replica_id: String,
    deployment_id: String,
    address: Ipv4Addr,
    image: String,
}

pub(crate) async fn run_import(
    config: &jiji_config::Config,
    sessions: &BTreeMap<String, Arc<SshSession>>,
    network_plan: &NetworkPlan,
    dry_run: bool,
    yes: bool,
) -> anyhow::Result<()> {
    let engine = config.builder.engine;
    let paths = AgentPaths::default_for_project(&config.project);

    Ui::section("Cutover Assessment:");
    let mut assess_failures = Vec::new();
    for (name, session) in sessions {
        let server_plan = &network_plan.servers[name];
        match assess::assess_host(session, engine, config, server_plan, &paths).await {
            Ok(report) => assess::print_report(name, &report),
            Err(error) => assess_failures.push(format!("{name}: {error}")),
        }
    }
    for failure in &assess_failures {
        Ui::result_warn("unreachable", failure);
    }

    let candidates = discover_candidates(config, engine, sessions).await?;
    if candidates.is_empty() {
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
        return Ok(());
    }

    if !yes {
        if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
            anyhow::bail!(
                "Refusing to prompt for confirmation without a terminal attached. Pass --yes to confirm the import when running non-interactively (e.g. CI/CD)."
            );
        }
        let confirmed = Ui::confirm(
            &format!(
                "Import {} replica(s) as historical (Stopped) catalog records? This never marks \
                 anything active and never touches a live replica.",
                candidates.len()
            ),
            false,
        )?;
        if !confirmed {
            anyhow::bail!("Import cancelled.");
        }
    }

    commit_candidates(sessions, &config.project, candidates).await
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
            // Discovery above finds at most one pre-existing container per (service, server);
            // treat it as local_index 0 regardless of `scale`, matching every other single-
            // container-per-server lookup in this codebase (e.g. `restart::resolve_restart_image`).
            let replica_id =
                placement::replica_id_for(&config.project, service_name, server_plan_name, 0);
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
