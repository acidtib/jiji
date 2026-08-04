//! `jiji network assess`: read-only comparison of a host's current resources against the
//! distributed control plane, for operators deciding between clean teardown+setup and
//! `jiji network import` (Phase 8, "Clean Cutover, Optional Import, and Release"). Never mutates
//! anything -- every command it runs is either a plain read or a `2>/dev/null || true` probe.
//! Reuses `catalog.rs`/`membership.rs`'s established `{binary} catalog-export`/`membership-export
//! --state-dir` exec pattern (works even when the agent process isn't running yet, since both
//! read the durable store file directly) rather than the socket API, which requires a live agent.

use std::collections::BTreeSet;

use jiji_agent::catalog::{CatalogRecord, DeploymentState};
use jiji_agent::membership::{MembershipState, SignedMembership};
use jiji_agent::AgentPaths;
use jiji_config::{validate_config, ContainerEngine};
use jiji_network::{NetworkPlanner, ServerPlan};
use jiji_ssh::SshSession;
use jiji_tui::Ui;

use crate::commands::network::setup::network_dir;
use crate::{container_ops, placement, ssh_adapter};

pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    hosts: Option<&str>,
) -> anyhow::Result<()> {
    let start = std::env::current_dir()?;
    let (config, _path) = crate::config_loading::load_config_for_ssh(
        environment,
        config_file.map(std::path::Path::new),
        &start,
    )
    .await?;
    if !validate_config(&config).valid {
        anyhow::bail!("Configuration is invalid; fix it before running an assessment");
    }
    let ssh = config
        .ssh
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No `ssh:` section is configured"))?;
    let plan = NetworkPlanner::new()
        .plan(&config)
        .map_err(|error| anyhow::anyhow!("Could not build the network plan: {error}"))?;
    let selected = plan.select_hosts(&split(hosts))?;
    if selected.is_empty() {
        anyhow::bail!("No servers are configured. Add a `servers:` entry and retry.");
    }

    let engine = config.builder.engine;
    let paths = AgentPaths::default_for_project(&config.project);
    Ui::section("Cutover Assessment:");
    let mut failures = Vec::new();
    for server_plan in &selected {
        let name = &server_plan.name;
        let named_server = config.servers.get(name).ok_or_else(|| {
            anyhow::anyhow!("Server '{name}' selected by the network plan is not configured")
        })?;
        let options = ssh_adapter::connect_options(name, named_server, ssh)?;
        let session = match SshSession::connect(&options).await {
            Ok(session) => session,
            Err(error) => {
                failures.push(format!("{name}: {error}"));
                continue;
            }
        };
        let result = assess_host(&session, engine, &config, server_plan, &paths).await;
        session.close().await;
        match result {
            Ok(report) => print_report(name, &report),
            Err(error) => failures.push(format!("{name}: {error}")),
        }
    }
    for failure in &failures {
        Ui::result_warn("unreachable", failure);
    }
    if !failures.is_empty() {
        anyhow::bail!("Could not assess {} server(s)", failures.len());
    }
    Ok(())
}

struct HostReport {
    legacy_runtime_present: bool,
    new_control_plane_enrolled: bool,
    catalog_record_count: usize,
    address_capacity: u64,
    address_used: u64,
    importable: Vec<String>,
    already_migrated: Vec<String>,
    orphaned_containers: Vec<String>,
}

async fn assess_host(
    session: &SshSession,
    engine: ContainerEngine,
    config: &jiji_config::Config,
    server_plan: &ServerPlan,
    paths: &AgentPaths,
) -> anyhow::Result<HostReport> {
    let legacy_runtime_present = probe_legacy_runtime(session, &config.project).await?;
    let membership = fetch_membership(session, paths).await?;
    let new_control_plane_enrolled = membership.iter().any(|operation| {
        operation.record.server_name == server_plan.name
            && operation.record.state == MembershipState::Active
    });
    let catalog = fetch_catalog(session, paths).await?;
    let containers = container_ops::list_managed_containers(session, engine, &config.project)
        .await
        .unwrap_or_default();

    let mut importable = Vec::new();
    let mut already_migrated = Vec::new();
    let mut accounted_services: BTreeSet<&str> = BTreeSet::new();
    for (service_name, service) in &config.services {
        if !service.servers.iter().any(|s| s == &server_plan.name) {
            continue;
        }
        let has_old_container = containers.iter().any(|c| {
            c.service.as_deref() == Some(service_name.as_str())
                && c.server.as_deref() == Some(server_plan.name.as_str())
        });
        if !has_old_container {
            continue;
        }
        accounted_services.insert(service_name.as_str());
        let replica_id = placement::endpoint_replica_id(
            &config.project,
            service_name,
            service,
            &server_plan.name,
        )?;
        let has_catalog_record = catalog.iter().any(|record| record.replica_id == replica_id);
        if has_catalog_record {
            already_migrated.push(format!("{service_name} (replica {replica_id})"));
        } else {
            importable.push(format!("{service_name} (replica {replica_id})"));
        }
    }

    let orphaned_containers = containers
        .iter()
        .filter(|c| {
            c.server.as_deref() == Some(server_plan.name.as_str())
                && c.service
                    .as_deref()
                    .map(|service| !accounted_services.contains(service))
                    .unwrap_or(true)
        })
        .map(|c| c.name.clone())
        .collect();

    let address_capacity = server_plan
        .container_subnet
        .address_count()
        .saturating_sub(5);
    let address_used = catalog
        .iter()
        .filter(|record| {
            record.state != DeploymentState::Tombstoned
                && server_plan.container_subnet.contains(record.address)
        })
        .count() as u64;

    Ok(HostReport {
        legacy_runtime_present,
        new_control_plane_enrolled,
        catalog_record_count: catalog.len(),
        address_capacity,
        address_used,
        importable,
        already_migrated,
        orphaned_containers,
    })
}

fn print_report(name: &str, report: &HostReport) {
    Ui::result_ok(
        name,
        &format!(
            "legacy_runtime={} enrolled={} catalog_records={} addresses={}/{}",
            report.legacy_runtime_present,
            report.new_control_plane_enrolled,
            report.catalog_record_count,
            report.address_used,
            report.address_capacity,
        ),
    );
    for entry in &report.importable {
        Ui::say(
            &format!("{name}: importable -- {entry} (no catalog record yet)"),
            1,
        );
    }
    for entry in &report.already_migrated {
        Ui::say(&format!("{name}: already migrated -- {entry}"), 1);
    }
    for container in &report.orphaned_containers {
        Ui::say(
            &format!(
                "{name}: conflict -- container '{container}' is not eligible for import \
                 (its service is not configured with this server); remove it manually or via \
                 `jiji service remove`"
            ),
            1,
        );
    }
}

async fn probe_legacy_runtime(session: &SshSession, project: &str) -> anyhow::Result<bool> {
    let slug = jiji_network::systemd_unit_slug(project);
    let root = network_dir(&slug);
    let command = format!(
        "test -s {root}/generation -o -d {root}/dns-current -o -d {root}/service-nat-current && echo yes || echo no"
    );
    let result = session.execute(&command).await?;
    Ok(result.stdout.trim() == "yes")
}

async fn fetch_membership(
    session: &SshSession,
    paths: &AgentPaths,
) -> anyhow::Result<Vec<SignedMembership>> {
    let command = format!(
        "{} membership-export --state-dir {} 2>/dev/null || true",
        paths.binary_path.display(),
        paths.state_dir.display()
    );
    let result = session.execute(&command).await?;
    let trimmed = result.stdout.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_str(trimmed).unwrap_or_default())
}

async fn fetch_catalog(
    session: &SshSession,
    paths: &AgentPaths,
) -> anyhow::Result<Vec<CatalogRecord>> {
    let command = format!(
        "{} catalog-export --state-dir {} 2>/dev/null || true",
        paths.binary_path.display(),
        paths.state_dir.display()
    );
    let result = session.execute(&command).await?;
    let trimmed = result.stdout.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_str(trimmed).unwrap_or_default())
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
