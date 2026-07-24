use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use jiji_config::{load_config, validate_config, ContainerEngine, NamedServer};
use jiji_network::NetworkPlanner;
use jiji_ssh::{SshPool, SshSession};
use jiji_tui::Ui;

use crate::commands::deploy::select_target_endpoints;
use crate::{audit, container_ops, registry, ssh_adapter};

#[derive(Debug)]
enum PruneStepResult {
    Removed,
    AlreadyAbsent,
    Retained { reason: String },
    Failed { error: String },
}

pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    hosts: Option<&str>,
    services: Option<&str>,
    retain_override: Option<u32>,
) -> anyhow::Result<()> {
    Ui::section("Service Prune:");

    let start = std::env::current_dir()?;
    let config_path = config_file.map(std::path::Path::new);
    let (config, path) = load_config(environment, config_path, &start)?;
    Ui::say(&format!("Configuration loaded from: {}", path.display()), 1);

    let validation = validate_config(&config);
    if !validation.valid {
        Ui::error(&format!(
            "Configuration validation failed with {} error(s):",
            validation.errors.len()
        ));
        for e in &validation.errors {
            Ui::say(&format!("{}: {}", e.path, e.message), 1);
        }
        anyhow::bail!("Configuration is invalid; fix the errors above and try again");
    }

    let ssh = config.ssh.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "No `ssh:` section configured in {}. Add at least `ssh.user:` before running service prune.",
            path.display()
        )
    })?;

    let plan = NetworkPlanner::new()
        .plan(&config)
        .map_err(|error| anyhow::anyhow!("Could not build the private network plan: {error}"))?;

    let selected = select_target_endpoints(&plan, hosts, services)?;

    // Only build-produced image tags are jiji's to prune -- a service with a static `image:`
    // reference (no `build:`) points at an externally owned image nobody here versions.
    let mut prunable = Vec::new();
    let mut warned = BTreeSet::new();
    for endpoint in &selected {
        let service = config
            .services
            .get(&endpoint.service)
            .expect("network plan endpoints only reference configured services");
        if service.build.is_none() {
            if services.is_some() && warned.insert(endpoint.service.clone()) {
                Ui::warn(&format!(
                    "{}: has no `build:` configured, skipping (only jiji-built image tags are prunable)",
                    endpoint.service
                ));
            }
            continue;
        }
        prunable.push(*endpoint);
    }

    if prunable.is_empty() {
        Ui::say("No prunable services selected.", 1);
        return Ok(());
    }

    let server_names: BTreeSet<String> = prunable.iter().map(|e| e.server.clone()).collect();
    let mut named_servers: Vec<(String, NamedServer)> = server_names
        .iter()
        .map(|name| {
            let server = config.servers.get(name).cloned().ok_or_else(|| {
                anyhow::anyhow!(
                    "Server '{name}' referenced by a selected endpoint is not defined in configuration"
                )
            })?;
            Ok::<_, anyhow::Error>((name.clone(), server))
        })
        .collect::<Result<_, _>>()?;
    named_servers.sort_by(|a, b| a.0.cmp(&b.0));

    let pool = SshPool::new(ssh.max_concurrent_starts as usize);
    let mut connect_options = BTreeMap::new();
    for (name, server) in &named_servers {
        connect_options.insert(
            name.clone(),
            ssh_adapter::connect_options(name, server, &ssh)?,
        );
    }

    Ui::section("Connecting:");
    let operations: Vec<_> = named_servers
        .iter()
        .map(|(name, _)| connect_options.get(name).expect("inserted above").clone())
        .map(|options| move || async move { SshSession::connect(&options).await })
        .collect();
    let connections = pool.execute_concurrent(operations).await;

    let mut sessions: BTreeMap<String, Arc<SshSession>> = BTreeMap::new();
    let mut connection_failures = Vec::new();
    for ((name, server), connection) in named_servers.iter().zip(connections) {
        match connection {
            Ok(session) => {
                Ui::say(&format!("{name} ({}): connected", server.host), 1);
                sessions.insert(name.clone(), Arc::new(session));
            }
            Err(error) => {
                Ui::error(&format!("{name} ({}): {error}", server.host));
                connection_failures.push(name.clone());
            }
        }
    }
    if !connection_failures.is_empty() {
        close_all(&sessions).await;
        anyhow::bail!(
            "Could not connect to server(s): {}. Restore SSH access and retry.",
            connection_failures.join(", ")
        );
    }

    Ui::section("Pruning:");
    let engine = config.builder.engine;
    let registry_config = config.builder.registry.clone();
    let project = config.project.clone();
    let mut operations = Vec::with_capacity(prunable.len());
    for endpoint in &prunable {
        let identity = endpoint.identity.clone();
        let service_name = endpoint.service.clone();
        let session = sessions
            .get(&endpoint.server)
            .expect("connected above")
            .clone();
        let service = config
            .services
            .get(&service_name)
            .expect("checked above")
            .clone();
        let retain_n = retain_override.unwrap_or(service.retain) as usize;
        let repo = registry::repo_reference(&registry_config, &project, &service_name);

        operations.push(move || async move {
            let repo = match repo {
                Ok(repo) => repo,
                Err(error) => return (identity, Err(error.to_string())),
            };
            match prune_service_images(&session, engine, &repo, retain_n).await {
                Ok(steps) => (identity, Ok(steps)),
                Err(error) => (identity, Err(error.to_string())),
            }
        });
    }

    let results = pool.execute_concurrent(operations).await;

    let server_by_identity: BTreeMap<String, String> = prunable
        .iter()
        .map(|endpoint| (endpoint.identity.clone(), endpoint.server.clone()))
        .collect();
    let endpoint_outcomes = results.iter().map(|(identity, outcome)| {
        let succeeded = match outcome {
            Ok(steps) => !steps
                .iter()
                .any(|(_, result)| matches!(result, PruneStepResult::Failed { .. })),
            Err(_) => false,
        };
        (
            identity.clone(),
            server_by_identity
                .get(identity)
                .expect("every pruned identity was selected above")
                .clone(),
            succeeded,
        )
    });
    audit::record_endpoints_by_server(
        &sessions,
        &plan.project,
        "service_prune",
        None,
        endpoint_outcomes,
    )
    .await;
    close_all(&sessions).await;

    Ui::section("Prune Summary:");
    let mut removed_total = 0usize;
    let mut failures = 0usize;
    for (identity, outcome) in &results {
        match outcome {
            Ok(steps) => {
                let removed = steps
                    .iter()
                    .filter(|(_, r)| matches!(r, PruneStepResult::Removed))
                    .count();
                removed_total += removed;
                Ui::say(&format!("{identity}: {removed} image(s) removed"), 1);
                for (image, result) in steps {
                    match result {
                        PruneStepResult::Retained { reason } => {
                            Ui::say(&format!("{image}: retained ({reason})"), 2);
                        }
                        PruneStepResult::Failed { error } => {
                            Ui::error(&format!("  {image}: failed ({error})"));
                        }
                        PruneStepResult::Removed | PruneStepResult::AlreadyAbsent => {}
                    }
                }
                let step_failures = steps
                    .iter()
                    .filter(|(_, r)| matches!(r, PruneStepResult::Failed { .. }))
                    .count();
                if step_failures > 0 {
                    failures += 1;
                }
            }
            Err(error) => {
                Ui::error(&format!("{identity}: {error}"));
                failures += 1;
            }
        }
    }

    let server_count = prunable
        .iter()
        .map(|e| e.server.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    Ui::success(&format!(
        "\nPruned {removed_total} image(s) across {server_count} server(s)."
    ));

    if failures > 0 {
        anyhow::bail!("Prune failed for {failures} endpoint(s); see the summary above.");
    }

    Ok(())
}

/// Lists image IDs for `repo` and removes every one after the first `retain_n`, skipping any
/// still referenced by a container. Deliberately relies on the engine's own `images` listing
/// order (newest first for both Docker and Podman) rather than parsing `CreatedAt` -- that field's
/// format is not reliably sortable and differs subtly between the two engines.
async fn prune_service_images(
    session: &SshSession,
    engine: ContainerEngine,
    repo: &str,
    retain_n: usize,
) -> anyhow::Result<Vec<(String, PruneStepResult)>> {
    let command = format!("{engine} images --format '{{{{.ID}}}}' --filter reference={repo}");
    let result = session.execute(&command).await?;
    if !result.success {
        anyhow::bail!(
            "Could not list images for '{repo}' on {}: {}",
            session.host(),
            result.stderr.trim()
        );
    }
    let ids: Vec<String> = result
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();

    let mut steps = Vec::new();
    for id in ids.into_iter().skip(retain_n) {
        let referenced = container_ops::image_referenced_elsewhere(session, engine, &id).await?;
        if !referenced.is_empty() {
            steps.push((
                id,
                PruneStepResult::Retained {
                    reason: format!("still used by {}", referenced.join(", ")),
                },
            ));
            continue;
        }
        match container_ops::remove_image_if_present(session, engine, &id).await {
            Ok(true) => steps.push((id, PruneStepResult::Removed)),
            Ok(false) => steps.push((id, PruneStepResult::AlreadyAbsent)),
            Err(error) => steps.push((
                id,
                PruneStepResult::Failed {
                    error: error.to_string(),
                },
            )),
        }
    }
    Ok(steps)
}

async fn close_all(sessions: &BTreeMap<String, Arc<SshSession>>) {
    for session in sessions.values() {
        session.close().await;
    }
}
