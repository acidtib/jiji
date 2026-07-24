use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use jiji_config::{load_config, validate_config, ContainerEngine, NamedServer};
use jiji_network::{BackendSlot, NetworkPlanner};
use jiji_ssh::{SshPool, SshSession};
use jiji_tui::Ui;

use crate::commands::deploy::select_target_endpoints;
use crate::{
    audit, container_ops, container_runtime, proxy_routes, service_network, ssh_adapter,
    volume_teardown,
};

#[derive(Debug)]
enum RemoveStepResult {
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
    yes: bool,
    remove_volumes: bool,
) -> anyhow::Result<()> {
    Ui::section("Service Remove:");

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
            "No `ssh:` section configured in {}. Add at least `ssh.user:` before running service remove.",
            path.display()
        )
    })?;

    let plan = NetworkPlanner::new()
        .plan(&config)
        .map_err(|error| anyhow::anyhow!("Could not build the private network plan: {error}"))?;

    let selected = select_target_endpoints(&plan, hosts, services)?;
    let full_removal = hosts.is_none() && services.is_none();

    Ui::section("About to remove:");
    for endpoint in &selected {
        let service = config
            .services
            .get(&endpoint.service)
            .expect("network plan endpoints only reference configured services");
        let a = container_runtime::container_name(&plan.project, &endpoint.service, BackendSlot::A);
        let b = container_runtime::container_name(&plan.project, &endpoint.service, BackendSlot::B);
        Ui::say(
            &format!("{}: containers '{a}', '{b}'", endpoint.identity),
            1,
        );
        for route in proxy_routes::targets_for_service(
            &plan.project,
            &endpoint.service,
            service.proxy.as_ref(),
            endpoint,
            BackendSlot::A,
        ) {
            Ui::say(&format!("proxy route '{}'", route.route_name), 2);
        }
    }
    if remove_volumes {
        let mut printed = BTreeSet::new();
        for endpoint in &selected {
            if !printed.insert(endpoint.service.clone()) {
                continue;
            }
            for candidate in
                volume_teardown::compute_candidates_for_service(&config, &endpoint.service)
            {
                Ui::say(
                    &format!(
                        "volume '{}' (service '{}')",
                        candidate.name, candidate.service
                    ),
                    1,
                );
            }
        }
        Ui::warn(
            "--volumes will permanently delete jiji-owned named volumes (application data) for the services listed above.",
        );
    }
    if full_removal {
        Ui::warn(
            "No -H/--hosts or -S/--services filter was given: every configured service on every server will be removed.",
        );
    }

    if !yes {
        let confirmed = Ui::confirm(
            &format!(
                "Remove {} service instance(s) listed above?",
                selected.len()
            ),
            false,
        )?;
        if !confirmed {
            anyhow::bail!("Removal cancelled.");
        }
    }

    let server_names: BTreeSet<String> = selected.iter().map(|e| e.server.clone()).collect();
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

    Ui::section("Removing:");
    let engine = config.builder.engine;
    let mut operations = Vec::with_capacity(selected.len());
    for endpoint in &selected {
        let identity = endpoint.identity.clone();
        let endpoint = (*endpoint).clone();
        let session = sessions
            .get(&endpoint.server)
            .expect("connected above")
            .clone();
        let project = plan.project.clone();
        let plan = plan.clone();
        let service = config
            .services
            .get(&endpoint.service)
            .expect("network plan endpoints only reference configured services")
            .clone();
        let volume_candidates = if remove_volumes {
            volume_teardown::compute_candidates_for_service(&config, &endpoint.service)
        } else {
            Vec::new()
        };

        operations.push(move || async move {
            let mut steps = Vec::new();

            for slot in [BackendSlot::A, BackendSlot::B] {
                let name = container_runtime::container_name(&project, &endpoint.service, slot);
                let result = remove_container(&session, engine, &name).await;
                steps.push((format!("container '{name}'"), result));
            }

            for route in proxy_routes::targets_for_service(
                &project,
                &endpoint.service,
                service.proxy.as_ref(),
                &endpoint,
                BackendSlot::A,
            ) {
                let result =
                    match proxy_routes::remove_route(&session, engine, &route.route_name).await {
                        Ok(()) => RemoveStepResult::Removed,
                        Err(error) => RemoveStepResult::Failed {
                            error: error.to_string(),
                        },
                    };
                steps.push((format!("proxy route '{}'", route.route_name), result));
            }

            match service_network::deactivate_slot(&session, &plan, &endpoint.identity).await {
                Ok(()) => steps.push(("VIP mapping".to_string(), RemoveStepResult::Removed)),
                Err(error) => steps.push((
                    "VIP mapping".to_string(),
                    RemoveStepResult::Failed {
                        error: error.to_string(),
                    },
                )),
            }

            if !volume_candidates.is_empty() {
                match volume_teardown::discover(&session, engine, &volume_candidates, &project)
                    .await
                {
                    Ok(discovered) => {
                        match volume_teardown::remove(&session, engine, &discovered).await {
                            Ok(results) => {
                                for (name, removed) in results {
                                    let matching = discovered.iter().find(|v| v.name == name);
                                    let result = if removed {
                                        RemoveStepResult::Removed
                                    } else {
                                        match matching.and_then(|v| v.blocked_by.clone()) {
                                            Some(reason) => RemoveStepResult::Retained { reason },
                                            None => RemoveStepResult::AlreadyAbsent,
                                        }
                                    };
                                    steps.push((format!("volume '{name}'"), result));
                                }
                            }
                            Err(error) => steps.push((
                                "volumes".to_string(),
                                RemoveStepResult::Failed {
                                    error: error.to_string(),
                                },
                            )),
                        }
                    }
                    Err(error) => steps.push((
                        "volumes".to_string(),
                        RemoveStepResult::Failed {
                            error: error.to_string(),
                        },
                    )),
                }
            }

            (identity, steps)
        });
    }

    let results = pool.execute_concurrent(operations).await;

    let server_by_identity: BTreeMap<String, String> = selected
        .iter()
        .map(|endpoint| (endpoint.identity.clone(), endpoint.server.clone()))
        .collect();
    let endpoint_outcomes = results.iter().map(|(identity, steps)| {
        let succeeded = !steps
            .iter()
            .any(|(_, result)| matches!(result, RemoveStepResult::Failed { .. }));
        (
            identity.clone(),
            server_by_identity
                .get(identity)
                .expect("every removed identity was selected above")
                .clone(),
            succeeded,
        )
    });
    audit::record_endpoints_by_server(
        &sessions,
        &plan.project,
        "service_remove",
        None,
        endpoint_outcomes,
    )
    .await;
    close_all(&sessions).await;

    Ui::section("Remove Summary:");
    let mut failures = 0usize;
    for (identity, steps) in &results {
        Ui::say(&format!("{identity}:"), 1);
        let mut has_failure = false;
        for (resource, result) in steps {
            match result {
                RemoveStepResult::Removed => Ui::say(&format!("{resource}: removed"), 2),
                RemoveStepResult::AlreadyAbsent => {
                    Ui::say(&format!("{resource}: already absent"), 2)
                }
                RemoveStepResult::Retained { reason } => {
                    Ui::warn(&format!("  {resource}: retained ({reason})"))
                }
                RemoveStepResult::Failed { error } => {
                    Ui::error(&format!("  {resource}: failed ({error})"));
                    has_failure = true;
                }
            }
        }
        if has_failure {
            failures += 1;
        }
    }

    if failures > 0 {
        anyhow::bail!("Removal failed for {failures} endpoint(s); see the summary above.");
    }

    Ui::success("\nRemoval completed.");
    Ok(())
}

async fn remove_container(
    session: &SshSession,
    engine: ContainerEngine,
    name: &str,
) -> RemoveStepResult {
    if let Err(error) = container_ops::stop_if_running(session, engine, name).await {
        return RemoveStepResult::Failed {
            error: error.to_string(),
        };
    }
    match container_ops::remove_if_present(session, engine, name).await {
        Ok(true) => RemoveStepResult::Removed,
        Ok(false) => RemoveStepResult::AlreadyAbsent,
        Err(error) => RemoveStepResult::Failed {
            error: error.to_string(),
        },
    }
}

async fn close_all(sessions: &BTreeMap<String, Arc<SshSession>>) {
    for session in sessions.values() {
        session.close().await;
    }
}
