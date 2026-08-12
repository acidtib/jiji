use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use jiji_agent::api::{RequestBody, ResponseBody};
use jiji_agent::catalog::{DeploymentState, HealthState};
use jiji_config::{validate_config, ContainerEngine, NamedServer};
use jiji_network::NetworkPlanner;
use jiji_ssh::{SshPool, SshSession};
use jiji_tui::Ui;

use crate::commands::deploy::select_target_endpoints;
use crate::lock::{LockRequest, LockScope};
use crate::{audit, container_ops, container_runtime, proxy_routes, ssh_adapter, volume_teardown};

#[derive(Debug)]
enum RemoveStepResult {
    Removed,
    AlreadyAbsent,
    Retained { reason: String },
    Failed { error: String },
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    hosts: Option<&str>,
    services: Option<&str>,
    yes: bool,
    remove_volumes: bool,
    lock_timeout: u64,
    force_lock: bool,
) -> anyhow::Result<()> {
    Ui::section("Service Remove:");
    let started_at = std::time::Instant::now();

    let start = std::env::current_dir()?;
    let config_path = config_file.map(std::path::Path::new);
    let (config, path) =
        crate::config_loading::load_config_for_ssh(environment, config_path, &start).await?;
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
        Ui::say(
            &format!("{}: all catalog-managed deployments", endpoint.identity),
            1,
        );
        let dns_server =
            std::net::SocketAddr::new(plan.servers[&endpoint.server].dns_address.into(), 53);
        for route in proxy_routes::targets_for_service(
            &plan.project,
            &endpoint.service,
            service.proxy.as_ref(),
            dns_server,
        )? {
            let label = match &route.path_prefix {
                Some(prefix) => format!("{}{prefix}", route.host),
                None => route.host.clone(),
            };
            Ui::say(&format!("proxy route '{label}'"), 2);
        }
        for route in proxy_routes::tcp_targets_for_service(
            &plan.project,
            &endpoint.service,
            service.proxy.as_ref(),
            dns_server,
        )? {
            Ui::say(&format!("proxy route 'tcp:{}'", route.listen_port), 2);
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

    // Discover the replica IDs each selected endpoint actually owns before locking, so removal
    // locks exactly those replicas (same granularity as deploy/restart/rollback) rather than a
    // coarser whole-service lock. An endpoint with nothing owned takes no lock for it.
    let mut discover_operations = Vec::with_capacity(selected.len());
    for endpoint in &selected {
        let endpoint = (*endpoint).clone();
        let session = sessions
            .get(&endpoint.server)
            .expect("connected above")
            .clone();
        let project = plan.project.clone();
        discover_operations.push(move || async move {
            let records = crate::agent_client::catalog(&session, &project).await?;
            let replica_ids: BTreeSet<String> = records
                .into_iter()
                .filter(|record| {
                    record.service == endpoint.service
                        && record.owner_node_id == endpoint.server
                        && !matches!(
                            record.state,
                            DeploymentState::Stopped | DeploymentState::Tombstoned
                        )
                })
                .map(|record| record.replica_id)
                .collect();
            anyhow::Ok((endpoint.server.clone(), replica_ids))
        });
    }
    let mut lock_requests: Vec<LockRequest> = Vec::new();
    for result in pool.execute_concurrent(discover_operations).await {
        let (server, replica_ids) = result?;
        for replica_id in replica_ids {
            lock_requests.push(LockRequest::new(
                LockScope::LogicalReplica { replica_id },
                server.clone(),
            ));
        }
    }
    let proxy_hosts: BTreeSet<String> = selected
        .iter()
        .filter(|endpoint| config.services[&endpoint.service].proxy.is_some())
        .map(|endpoint| endpoint.server.clone())
        .collect();
    for host in proxy_hosts {
        lock_requests.push(LockRequest::new(LockScope::HostGlobalProxy, host));
    }

    let remove_result = crate::commands::lock::with_locks(
        &pool,
        &sessions,
        &plan.project,
        lock_requests,
        format!(
            "jiji service remove: {}",
            selected
                .iter()
                .map(|endpoint| endpoint.identity.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        crate::commands::lock::AutomaticLockOptions {
            timeout: lock_timeout,
            force: force_lock,
        },
        || async {
            Ui::section("Removing:");
            let ops: Vec<(String, String)> = selected
                .iter()
                .map(|e| (e.identity.clone(), e.server.clone()))
                .collect();
            let progress =
                jiji_tui::DeployProgress::with_servers_and_title(ops, "Removing".to_string());
            let handle = progress.handle();
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
                let dns_server = std::net::SocketAddr::new(
                    plan.servers[&endpoint.server].dns_address.into(),
                    53,
                );
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

                let h = handle.clone();
                let id_for_status = identity.clone();
                operations.push(move || {
                    let h = h.clone();
                    let id_for_status = id_for_status.clone();
                    async move {
                        h.set_status(&id_for_status, "removing");
                        let mut steps = Vec::new();
                        let mut retired_deployment_ids: Vec<String> = Vec::new();

                        let records = match crate::agent_client::catalog(&session, &project).await {
                            Ok(records) => records,
                            Err(error) => {
                                h.mark_failed(&id_for_status, &error.to_string());
                                steps.push((
                                    "service catalog".to_string(),
                                    RemoveStepResult::Failed {
                                        error: error.to_string(),
                                    },
                                ));
                                return (identity, steps, retired_deployment_ids);
                            }
                        };
                        let owned = records
                            .into_iter()
                            .filter(|record| {
                                record.service == endpoint.service
                                    && record.owner_node_id == endpoint.server
                                    && !matches!(
                                        record.state,
                                        DeploymentState::Stopped | DeploymentState::Tombstoned
                                    )
                            })
                            .collect::<Vec<_>>();
                        if owned.is_empty() {
                            steps.push((
                                "catalog deployments".to_string(),
                                RemoveStepResult::AlreadyAbsent,
                            ));
                        }
                        for record in owned {
                            let name = container_runtime::dynamic_container_name(
                                &project,
                                &endpoint.service,
                                &record.deployment_id,
                            );
                            let result = remove_container(&session, engine, &name).await;
                            let removed = !matches!(result, RemoveStepResult::Failed { .. });
                            steps.push((format!("container '{name}'"), result));
                            if removed {
                                let retire_result =
                                    retire_deployment(&session, &project, &record).await;
                                if matches!(retire_result, RemoveStepResult::Removed) {
                                    retired_deployment_ids.push(record.deployment_id.clone());
                                }
                                steps.push((
                                    format!("catalog deployment '{}'", record.deployment_id),
                                    retire_result,
                                ));
                            }
                        }

                        let route_targets = proxy_routes::targets_for_service(
                            &project,
                            &endpoint.service,
                            service.proxy.as_ref(),
                            dns_server,
                        );
                        match route_targets {
                            Ok(route_targets) => {
                                for route in route_targets {
                                    let label = match &route.path_prefix {
                                        Some(prefix) => format!("{}{prefix}", route.host),
                                        None => route.host.clone(),
                                    };
                                    let result = match proxy_routes::remove_route(
                                        &session,
                                        engine,
                                        &route.host,
                                        route.path_prefix.as_deref(),
                                    )
                                    .await
                                    {
                                        Ok(()) => RemoveStepResult::Removed,
                                        Err(error) => RemoveStepResult::Failed {
                                            error: error.to_string(),
                                        },
                                    };
                                    steps.push((format!("proxy route '{label}'"), result));
                                }
                            }
                            Err(error) => {
                                steps.push((
                                    "proxy routes".to_string(),
                                    RemoveStepResult::Failed {
                                        error: error.to_string(),
                                    },
                                ));
                            }
                        }

                        let tcp_route_targets = proxy_routes::tcp_targets_for_service(
                            &project,
                            &endpoint.service,
                            service.proxy.as_ref(),
                            dns_server,
                        );
                        match tcp_route_targets {
                            Ok(tcp_route_targets) => {
                                for route in tcp_route_targets {
                                    let result = match proxy_routes::remove_tcp_route(
                                        &session,
                                        engine,
                                        route.listen_port,
                                    )
                                    .await
                                    {
                                        Ok(()) => RemoveStepResult::Removed,
                                        Err(error) => RemoveStepResult::Failed {
                                            error: error.to_string(),
                                        },
                                    };
                                    steps.push((
                                        format!("proxy route 'tcp:{}'", route.listen_port),
                                        result,
                                    ));
                                }
                            }
                            Err(error) => {
                                steps.push((
                                    "tcp proxy routes".to_string(),
                                    RemoveStepResult::Failed {
                                        error: error.to_string(),
                                    },
                                ));
                            }
                        }

                        if !volume_candidates.is_empty() {
                            match volume_teardown::discover(
                                &session,
                                engine,
                                &volume_candidates,
                                &project,
                            )
                            .await
                            {
                                Ok(discovered) => {
                                    match volume_teardown::remove(&session, engine, &discovered)
                                        .await
                                    {
                                        Ok(results) => {
                                            for (name, removed) in results {
                                                let matching =
                                                    discovered.iter().find(|v| v.name == name);
                                                let result = if removed {
                                                    RemoveStepResult::Removed
                                                } else {
                                                    match matching
                                                        .and_then(|v| v.blocked_by.clone())
                                                    {
                                                        Some(reason) => {
                                                            RemoveStepResult::Retained { reason }
                                                        }
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

                        let _is_ok = !steps
                            .iter()
                            .any(|(_, r)| matches!(r, RemoveStepResult::Failed { .. }));
                        // Live dashboard: one line per endpoint, with timing; keep detail short.
                        if _is_ok {
                            h.mark_success(&id_for_status, "removed");
                        } else {
                            let first_err = steps
                                .iter()
                                .find_map(|(_, r)| match r {
                                    RemoveStepResult::Failed { error } => Some(error.as_str()),
                                    _ => None,
                                })
                                .unwrap_or("failed");
                            h.mark_failed(&id_for_status, first_err);
                        }
                        (identity, steps, retired_deployment_ids)
                    }
                });
            }

            let results = pool.execute_concurrent(operations).await;
            progress.finish();

            let server_by_identity: BTreeMap<String, String> = selected
                .iter()
                .map(|endpoint| (endpoint.identity.clone(), endpoint.server.clone()))
                .collect();
            let endpoint_outcomes =
                results
                    .iter()
                    .map(|(identity, steps, retired_deployment_ids)| {
                        let succeeded = !steps
                            .iter()
                            .any(|(_, result)| matches!(result, RemoveStepResult::Failed { .. }));
                        let deployment_id = match retired_deployment_ids.as_slice() {
                            [deployment_id] => Some(deployment_id.clone()),
                            _ => None,
                        };
                        (
                            identity.clone(),
                            server_by_identity
                                .get(identity)
                                .expect("every removed identity was selected above")
                                .clone(),
                            succeeded,
                            deployment_id,
                        )
                    });
            audit::record_endpoints_by_server(
                &sessions,
                &plan.project,
                "service_remove",
                None,
                endpoint_outcomes,
                true,
                Some(started_at.elapsed()),
            )
            .await;

            let mut cron_services: BTreeSet<&str> = BTreeSet::new();
            for endpoint in &selected {
                cron_services.insert(endpoint.service.as_str());
            }
            let mut cron_problems = Vec::new();
            let mut image_retention_problems = Vec::new();
            for service_name in cron_services {
                let service = &config.services[service_name];
                cron_problems.extend(
                    crate::cron_reconcile::remove_all_cron_specs(
                        &ssh,
                        &config,
                        service_name,
                        service,
                        &sessions,
                    )
                    .await,
                );
                image_retention_problems.extend(
                    crate::image_retention_reconcile::remove_all_retention_specs(
                        &ssh,
                        &config,
                        service_name,
                        service,
                        &sessions,
                    )
                    .await,
                );
            }

            Ui::section("Remove Summary:");
            let mut failures = 0usize;
            for (identity, steps, _) in &results {
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

            for problem in &cron_problems {
                Ui::error(&format!("cron: {problem}"));
                failures += 1;
            }
            for problem in &image_retention_problems {
                Ui::error(&format!("image-retention: {problem}"));
                failures += 1;
            }

            if failures > 0 {
                anyhow::bail!("Removal failed for {failures} endpoint(s); see the summary above.");
            }

            Ok(())
        },
    )
    .await;
    close_all(&sessions).await;
    remove_result?;
    Ui::success_elapsed("Removal completed.", started_at.elapsed());
    Ok(())
}

async fn retire_deployment(
    session: &SshSession,
    project: &str,
    record: &jiji_agent::catalog::CatalogRecord,
) -> RemoveStepResult {
    let committed = crate::agent_client::call(
        session,
        project,
        Some(format!("remove:catalog:{}", record.deployment_id)),
        RequestBody::CatalogCommit {
            service: record.service.clone(),
            replica_id: record.replica_id.clone(),
            deployment_id: record.deployment_id.clone(),
            address: record.address.to_string(),
            ports: record.ports.clone(),
            image: record.image.clone(),
            state: DeploymentState::Tombstoned,
            health: HealthState::Unhealthy,
        },
    )
    .await;
    if let Err(error) = committed {
        return RemoveStepResult::Failed {
            error: error.to_string(),
        };
    }
    if !matches!(committed, Ok(ResponseBody::CatalogCommitted { .. })) {
        return RemoveStepResult::Failed {
            error: "agent returned an unexpected catalog response".to_string(),
        };
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    match crate::agent_client::call(
        session,
        project,
        Some(format!("remove:lease:{}", record.deployment_id)),
        RequestBody::ReleaseAddress {
            deployment_id: record.deployment_id.clone(),
            timestamp,
        },
    )
    .await
    {
        Ok(ResponseBody::AddressReleased { .. }) => RemoveStepResult::Removed,
        Ok(_) => RemoveStepResult::Failed {
            error: "agent returned an unexpected address-release response".to_string(),
        },
        Err(error) => RemoveStepResult::Failed {
            error: error.to_string(),
        },
    }
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
