use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::Context;
use jiji_agent::catalog::{CatalogRecord, DeploymentState, HealthState};
use jiji_config::{validate_config, Config, NamedServer, Ssh};
use jiji_network::{NetworkPlan, NetworkPlanner};
use jiji_ssh::{SshPool, SshSession};
use jiji_tui::Ui;

use crate::commands::deploy::{select_target_endpoints, split_comma_trimmed};
use crate::commands::proxy::logs::{effective_lines, render_logs_command, stream_logs};
use crate::{container_runtime, ssh_adapter};

pub struct LogsOptions<'a> {
    pub environment: Option<&'a str>,
    pub config_file: Option<&'a str>,
    pub hosts: Option<&'a str>,
    pub services: Option<&'a str>,
    pub lines: Option<u32>,
    pub since: Option<&'a str>,
    pub grep: Option<&'a str>,
    pub grep_options: Option<&'a str>,
    pub follow: bool,
    pub container_id: Option<&'a str>,
}

pub async fn run(options: LogsOptions<'_>) -> anyhow::Result<()> {
    let LogsOptions {
        environment,
        config_file,
        hosts,
        services,
        lines,
        since,
        grep,
        grep_options,
        follow,
        container_id,
    } = options;

    Ui::section("Service Logs:");
    if container_id.is_some() && services.is_some() {
        anyhow::bail!(
            "`jiji service logs --container-id` does not accept -S/--services: an arbitrary container name isn't scoped to a configured service. Use -H/--hosts to select servers instead."
        );
    }
    let start = std::env::current_dir()?;
    let (config, path) = crate::config_loading::load_config_for_ssh(
        environment,
        config_file.map(std::path::Path::new),
        &start,
    )
    .await?;
    let validation = validate_config(&config);
    if !validation.valid {
        for error in validation.errors {
            Ui::error(&format!("{}: {}", error.path, error.message));
        }
        anyhow::bail!("Configuration is invalid; fix the errors above and try again");
    }
    let ssh = config.ssh.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "No `ssh:` section configured in {}. Add at least `ssh.user:` before running service logs.",
            path.display()
        )
    })?;
    let plan = NetworkPlanner::new()
        .plan(&config)
        .map_err(|error| anyhow::anyhow!("Could not build the private network plan: {error}"))?;

    let effective_lines = effective_lines(lines, since, grep);

    if let Some(container_id) = container_id {
        return run_container_id(
            &config,
            &plan,
            &ssh,
            hosts,
            container_id,
            effective_lines,
            since,
            grep,
            grep_options,
            follow,
        )
        .await;
    }

    let selected = select_target_endpoints(&plan, hosts, services)?;
    if follow && selected.len() != 1 {
        anyhow::bail!(
            "-H/--hosts and -S/--services matched {} target(s) ({}). `jiji service logs --follow` requires exactly one; narrow the filters and try again.",
            selected.len(),
            selected
                .iter()
                .map(|endpoint| endpoint.identity.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let server_names: BTreeSet<String> = selected.iter().map(|e| e.server.clone()).collect();
    let sessions = connect_sessions(&config, &ssh, &server_names).await?;

    if follow {
        let endpoint = selected[0];
        let session = sessions.get(&endpoint.server).expect("connected above");
        let result = follow_endpoint(
            session,
            &plan,
            endpoint,
            config
                .services
                .get(&endpoint.service)
                .expect("selected service is configured"),
            config.builder.engine,
            effective_lines,
            since,
            grep,
            grep_options,
        )
        .await;
        close_all(&sessions).await;
        return result;
    }

    // Catalog state is cached per server: every local agent exposes the converged project view,
    // and several selected replicas commonly share one SSH session.
    let mut catalog_cache: BTreeMap<String, Vec<CatalogRecord>> = BTreeMap::new();
    let mut failures = Vec::new();
    for endpoint in &selected {
        let session = sessions.get(&endpoint.server).expect("connected above");
        if !catalog_cache.contains_key(&endpoint.server) {
            match crate::agent_client::catalog(session, &plan.project).await {
                Ok(records) => {
                    catalog_cache.insert(endpoint.server.clone(), records);
                }
                Err(error) => {
                    Ui::error(&format!("{}: {error}", endpoint.identity));
                    failures.push(endpoint.identity.clone());
                    continue;
                }
            }
        }
        let service = config
            .services
            .get(&endpoint.service)
            .expect("selected service is configured");
        let replica_id = crate::placement::endpoint_replica_id(
            &plan.project,
            &endpoint.service,
            service,
            &endpoint.server,
        )?;
        let active = catalog_cache
            .get(&endpoint.server)
            .expect("inserted above")
            .iter()
            .find(|record| {
                record.replica_id == replica_id
                    && record.state == DeploymentState::Active
                    && record.health == HealthState::Healthy
            });
        let Some(active) = active else {
            Ui::warn(&format!(
                "{}: no active container, skipping",
                endpoint.identity
            ));
            continue;
        };
        let container = container_runtime::dynamic_container_name(
            &plan.project,
            &endpoint.service,
            &active.deployment_id,
        );
        let command = render_logs_command(
            config.builder.engine,
            &container,
            effective_lines,
            since,
            grep,
            grep_options,
            false,
        );
        match session.execute(&command).await {
            Ok(result) if result.success => {
                Ui::say(&format!("{}:", endpoint.identity), 1);
                if !result.stdout.is_empty() {
                    print!("{}", result.stdout);
                }
                if !result.stderr.is_empty() {
                    eprint!("{}", result.stderr);
                }
            }
            Ok(result) => {
                let error = format!(
                    "remote logs command failed with status {:?}: {}",
                    result.code,
                    result.stderr.trim()
                );
                Ui::error(&format!("{}: {error}", endpoint.identity));
                failures.push(endpoint.identity.clone());
            }
            Err(error) => {
                Ui::error(&format!("{}: {error}", endpoint.identity));
                failures.push(endpoint.identity.clone());
            }
        }
    }
    close_all(&sessions).await;
    if !failures.is_empty() {
        anyhow::bail!(
            "Could not read logs for {} target(s). Fix the reported services/hosts and retry `jiji service logs`.",
            failures.len()
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn follow_endpoint(
    session: &SshSession,
    plan: &NetworkPlan,
    endpoint: &jiji_network::ServiceEndpointPlan,
    service: &jiji_config::Service,
    engine: jiji_config::ContainerEngine,
    lines: Option<u32>,
    since: Option<&str>,
    grep: Option<&str>,
    grep_options: Option<&str>,
) -> anyhow::Result<()> {
    let replica_id = crate::placement::endpoint_replica_id(
        &plan.project,
        &endpoint.service,
        service,
        &endpoint.server,
    )?;
    let active = crate::agent_client::catalog(session, &plan.project)
        .await?
        .into_iter()
        .find(|record| {
            record.replica_id == replica_id
                && record.state == DeploymentState::Active
                && record.health == HealthState::Healthy
        });
    let Some(active) = active else {
        anyhow::bail!(
            "Service '{}' has no active container on '{}'. Deploy it first with `jiji deploy`.",
            endpoint.service,
            endpoint.server
        );
    };
    let container = container_runtime::dynamic_container_name(
        &plan.project,
        &endpoint.service,
        &active.deployment_id,
    );
    let command = render_logs_command(engine, &container, lines, since, grep, grep_options, true);
    stream_logs(session, &command).await
}

#[allow(clippy::too_many_arguments)]
async fn run_container_id(
    config: &Config,
    plan: &NetworkPlan,
    ssh: &Ssh,
    hosts: Option<&str>,
    container_id: &str,
    lines: Option<u32>,
    since: Option<&str>,
    grep: Option<&str>,
    grep_options: Option<&str>,
    follow: bool,
) -> anyhow::Result<()> {
    let selected = plan.select_hosts(&split_comma_trimmed(hosts))?;
    if selected.is_empty() {
        anyhow::bail!("No servers are configured. Add a `servers:` entry and retry.");
    }
    if follow && selected.len() != 1 {
        anyhow::bail!(
            "-H/--hosts matched {} servers ({}). `jiji service logs --follow` requires exactly one host; narrow the filter and try again.",
            selected.len(),
            selected
                .iter()
                .map(|server| server.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let command = render_logs_command(
        config.builder.engine,
        container_id,
        lines,
        since,
        grep,
        grep_options,
        follow,
    );

    if follow {
        let target = selected[0];
        let named_server = config.servers.get(&target.name).ok_or_else(|| {
            anyhow::anyhow!(
                "Server '{}' selected by the network plan is not configured",
                target.name
            )
        })?;
        let options = ssh_adapter::connect_options(&target.name, named_server, ssh)?;
        let session = SshSession::connect(&options)
            .await
            .with_context(|| format!("Could not connect to '{}'", target.name))?;
        let result = stream_logs(&session, &command).await;
        session.close().await;
        return result;
    }

    let mut operations = Vec::with_capacity(selected.len());
    for target in selected {
        let name = target.name.clone();
        let named_server = config.servers.get(&name).ok_or_else(|| {
            anyhow::anyhow!("Server '{name}' selected by the network plan is not configured")
        })?;
        let options = ssh_adapter::connect_options(&name, named_server, ssh)?;
        let command = command.clone();
        operations.push(move || async move {
            let result = async {
                let session = SshSession::connect(&options).await?;
                let outcome = session.execute(&command).await;
                session.close().await;
                outcome
            }
            .await;
            (name, result)
        });
    }
    let pool = SshPool::new(ssh.max_concurrent_starts as usize);
    let outcomes = pool.execute_concurrent(operations).await;
    let mut failures = Vec::new();
    for (name, outcome) in outcomes {
        match outcome {
            Ok(result) if result.success => {
                Ui::say(&format!("{name}:"), 1);
                if !result.stdout.is_empty() {
                    print!("{}", result.stdout);
                }
                if !result.stderr.is_empty() {
                    eprint!("{}", result.stderr);
                }
            }
            Ok(result) => {
                let error = format!(
                    "remote logs command failed with status {:?}: {}",
                    result.code,
                    result.stderr.trim()
                );
                Ui::error(&format!("{name}: {error}"));
                failures.push(error);
            }
            Err(error) => {
                Ui::error(&format!("{name}: {error}"));
                failures.push(error.to_string());
            }
        }
    }
    if !failures.is_empty() {
        anyhow::bail!(
            "Could not read logs from {} server(s). Fix the reported hosts and retry `jiji service logs`.",
            failures.len()
        );
    }
    Ok(())
}

async fn connect_sessions(
    config: &Config,
    ssh: &Ssh,
    server_names: &BTreeSet<String>,
) -> anyhow::Result<BTreeMap<String, Arc<SshSession>>> {
    let mut named_servers: Vec<(String, NamedServer)> = Vec::new();
    for name in server_names {
        let server = config.servers.get(name).cloned().ok_or_else(|| {
            anyhow::anyhow!(
                "Server '{name}' referenced by a selected endpoint is not defined in configuration"
            )
        })?;
        named_servers.push((name.clone(), server));
    }

    let pool = SshPool::new(ssh.max_concurrent_starts as usize);
    let mut connect_options = BTreeMap::new();
    for (name, server) in &named_servers {
        connect_options.insert(
            name.clone(),
            ssh_adapter::connect_options(name, server, ssh)?,
        );
    }
    let operations: Vec<_> = named_servers
        .iter()
        .map(|(name, _)| connect_options.get(name).expect("inserted above").clone())
        .map(|options| move || async move { SshSession::connect(&options).await })
        .collect();
    let connections = pool.execute_concurrent(operations).await;

    let mut sessions = BTreeMap::new();
    let mut failures = Vec::new();
    for ((name, _), connection) in named_servers.iter().zip(connections) {
        match connection {
            Ok(session) => {
                sessions.insert(name.clone(), Arc::new(session));
            }
            Err(error) => failures.push(format!("{name}: {error}")),
        }
    }
    if !failures.is_empty() {
        close_all(&sessions).await;
        anyhow::bail!(
            "Could not connect to server(s): {}. Restore SSH access and retry.",
            failures.join(", ")
        );
    }
    Ok(sessions)
}

async fn close_all(sessions: &BTreeMap<String, Arc<SshSession>>) {
    for session in sessions.values() {
        session.close().await;
    }
}
