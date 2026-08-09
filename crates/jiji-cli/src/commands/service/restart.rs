use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::Context;
use jiji_agent::catalog::{DeploymentState, HealthState};
use jiji_config::{validate_config, NamedServer, Service};
use jiji_network::{NetworkPlan, NetworkPlanner, ServiceEndpointPlan};
use jiji_ssh::{SshPool, SshSession};
use jiji_tui::Ui;

use crate::cascade::{add_cascaded_dependents, compute_service_waves, deploy_service_endpoints};
use crate::commands::deploy::select_target_endpoints;
use crate::deploy_transaction::EndpointOutcome;
use crate::lock::{LockRequest, LockScope};
use crate::{audit, container_runtime, env_resolution, proxy, ssh_adapter};

/// Zero-downtime deployment replacement built on the same catalog transaction as `jiji deploy`,
/// reusing whatever image is already configured/running rather than building or bumping a
/// version.
pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    hosts: Option<&str>,
    services: Option<&str>,
    host_env: bool,
    lock_timeout: u64,
    force_lock: bool,
) -> anyhow::Result<()> {
    Ui::section("Service Restart:");
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
            "No `ssh:` section configured in {}. Add at least `ssh.user:` before running service restart.",
            path.display()
        )
    })?;

    let plan = NetworkPlanner::new()
        .plan(&config)
        .context("Could not build the private network plan")?;
    if !plan.enabled {
        anyhow::bail!(
            "Private networking is disabled in configuration; `jiji service restart` requires it. Enable `network.enabled` and run `jiji server setup`."
        );
    }

    let mut selected: Vec<ServiceEndpointPlan> = select_target_endpoints(&plan, hosts, services)?
        .into_iter()
        .cloned()
        .collect();
    let mut replica_ids: BTreeMap<String, String> = BTreeMap::new();
    for endpoint in &selected {
        let service = config
            .services
            .get(&endpoint.service)
            .expect("checked by select_target_endpoints");
        let replica_id = crate::placement::endpoint_replica_id(
            &plan.project,
            &endpoint.service,
            service,
            &endpoint.server,
        )
        .expect("selected endpoint is eligible for its service");
        replica_ids.insert(endpoint.identity.clone(), replica_id);
    }
    // A restarted upstream's `network_mode: service:<upstream>` dependents must be restarted too
    // in the same invocation, in the same order `jiji deploy` cascades them -- see
    // `add_cascaded_dependents`.
    add_cascaded_dependents(&config, &plan, &mut selected, &mut replica_ids)?;
    Ui::say(
        &format!(
            "Restarting {} endpoint(s): {}",
            selected.len(),
            selected
                .iter()
                .map(|e| e.identity.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        1,
    );

    let service_names = selected
        .iter()
        .map(|endpoint| endpoint.service.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(", ");

    // Connect once, before locking: only the servers actually hosting a selected endpoint, never
    // every configured server (unlike locking by raw `-H`, which resolves an omitted filter to
    // every server).
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

    let mut lock_requests: Vec<LockRequest> = Vec::new();
    for endpoint in &selected {
        let service = config
            .services
            .get(&endpoint.service)
            .expect("checked above when connecting");
        let replica_id = replica_ids
            .get(&endpoint.identity)
            .expect("computed above")
            .clone();
        lock_requests.push(LockRequest::new(
            LockScope::LogicalReplica { replica_id },
            endpoint.server.clone(),
        ));
        if service.proxy.is_some() {
            lock_requests.push(LockRequest::new(
                LockScope::HostGlobalProxy,
                endpoint.server.clone(),
            ));
        }
    }

    let restart_result = crate::commands::lock::with_locks(
        &pool,
        &sessions,
        &config.project,
        lock_requests,
        format!("jiji service restart: {service_names}"),
        crate::commands::lock::AutomaticLockOptions {
            timeout: lock_timeout,
            force: force_lock,
        },
        || async {
            // Restart still performs a VIP cutover, so the installed network generation must be current
            // first -- same precondition `jiji deploy` enforces.
            crate::commands::network::setup::reconcile_for_deploy(&config, &plan).await?;

            let project_root = env_resolution::project_root_from_config_path(&path);
            let (loaded_env, loaded_from) = env_resolution::load_env_file(
                &project_root,
                environment,
                config.secrets_path.as_deref(),
            )?;
            if let Some(loaded_from) = &loaded_from {
                Ui::say(
                    &format!("Environment loaded from: {}", loaded_from.display()),
                    1,
                );
            }

            let shared_env = config.environment.clone().unwrap_or_default();
            let mut resolved_envs: BTreeMap<String, env_resolution::ResolvedEnvironment> =
                BTreeMap::new();
            for endpoint in &selected {
                if resolved_envs.contains_key(&endpoint.service) {
                    continue;
                }
                let service = config.services.get(&endpoint.service).ok_or_else(|| {
                    anyhow::anyhow!(
                        "Service '{}' is not defined in configuration",
                        endpoint.service
                    )
                })?;
                let merged = env_resolution::merge_environment(&shared_env, &service.environment);
                let resolved = env_resolution::resolve_environment(&merged, &loaded_env, host_env)
                    .with_context(|| {
                        format!(
                            "Could not resolve environment for service '{}'",
                            endpoint.service
                        )
                    })?;
                resolved_envs.insert(endpoint.service.clone(), resolved);
            }

            Ui::section("Verifying Proxy:");
            for (server_name, session) in &sessions {
                let serves_proxy = selected.iter().any(|endpoint| {
                    &endpoint.server == server_name
                        && config
                            .services
                            .get(&endpoint.service)
                            .and_then(|service| service.proxy.as_ref())
                            .is_some()
                });
                if !serves_proxy {
                    continue;
                }
                let server_plan = &plan.servers[server_name];
                let network = Some(proxy::ProxyNetwork {
                    bridge_name: server_plan.bridge_name.clone(),
                    bridge_interface: server_plan.bridge_interface.clone(),
                    proxy_address: server_plan.proxy_address,
                    dns_address: server_plan.dns_address,
                    public_host: proxy::parse_public_host(server_plan)?,
                });
                if let Err(error) =
                    proxy::ensure_proxy(session, config.builder.engine, network, false).await
                {
                    return Err(
                        error.context(format!("jiji-proxy is not ready on '{server_name}'"))
                    );
                }
            }

            // Resolved per endpoint identity, not per service: a build-only service's currently-running
            // image is discovered by inspecting that specific replica's active container, so different
            // replicas of the same service could in principle be restarted from different images if a
            // prior deploy only reached some of them. Run concurrently through the same pool used for
            // connecting/restarting -- each lookup is an independent SSH round trip.
            Ui::section("Resolving Images:");
            let mut image_operations = Vec::with_capacity(selected.len());
            for endpoint in &selected {
                let identity = endpoint.identity.clone();
                let endpoint = (*endpoint).clone();
                let session = sessions
                    .get(&endpoint.server)
                    .expect("connected above")
                    .clone();
                let plan_for_image = plan.clone();
                let service = config
                    .services
                    .get(&endpoint.service)
                    .expect("checked above")
                    .clone();
                image_operations.push(move || async move {
                    let result =
                        resolve_restart_image(&session, &plan_for_image, &endpoint, &service).await;
                    (identity, result)
                });
            }
            let image_results = pool.execute_concurrent(image_operations).await;

            let mut images: BTreeMap<String, String> = BTreeMap::new();
            let mut image_failures = Vec::new();
            for (identity, result) in image_results {
                match result {
                    Ok(image) => {
                        Ui::say(&format!("{identity}: {image}"), 1);
                        images.insert(identity, image);
                    }
                    Err(error) => {
                        Ui::error(&format!("{identity}: {error}"));
                        image_failures.push(error.to_string());
                    }
                }
            }
            if !image_failures.is_empty() {
                anyhow::bail!(
                    "Could not resolve the restart image for {} endpoint(s); see the errors above.",
                    image_failures.len()
                );
            }

            let engine = config.builder.engine;
            let restart_spinner =
                Ui::spinner(&format!("Restarting {} endpoint(s)", selected.len()));
            let mut endpoints_by_service: BTreeMap<String, Vec<ServiceEndpointPlan>> =
                BTreeMap::new();
            for endpoint in &selected {
                endpoints_by_service
                    .entry(endpoint.service.clone())
                    .or_default()
                    .push((*endpoint).clone());
            }

            // A restarted upstream's cascaded dependents must be restarted strictly after it, in
            // a second dispatch wave -- see `compute_service_waves`.
            let (dependents_of, wave_one, wave_two) =
                compute_service_waves(&config, endpoints_by_service);

            let build_wave =
                |endpoints_by_service: BTreeMap<String, Vec<ServiceEndpointPlan>>,
                 presumed_failed: &BTreeMap<String, bool>| {
                    endpoints_by_service
                        .into_iter()
                        .map(|(service_name, endpoints)| {
                            let service = config
                                .services
                                .get(&service_name)
                                .expect("checked above")
                                .clone();
                            let resolved_env = resolved_envs
                                .get(&service_name)
                                .expect("resolved above")
                                .clone();
                            let plan = plan.clone();
                            let sessions = sessions.clone();
                            let images = images.clone();
                            let replica_ids = replica_ids.clone();
                            let project_root = project_root.clone();
                            let progress = restart_spinner.handle();
                            let force_skip_proxy = dependents_of.contains_key(&service_name);
                            let presumed_failed = presumed_failed
                                .get(
                                    config.services[&service_name]
                                        .network_mode_dependency()
                                        .unwrap_or_default(),
                                )
                                .copied()
                                .unwrap_or(false);

                            move || {
                                deploy_service_endpoints(
                                    sessions,
                                    plan,
                                    replica_ids,
                                    service_name,
                                    service,
                                    images,
                                    resolved_env,
                                    project_root,
                                    engine,
                                    force_skip_proxy,
                                    endpoints,
                                    progress,
                                    presumed_failed,
                                    "Restarting",
                                )
                            }
                        })
                        .collect::<Vec<_>>()
                };

            let mutation_pool = SshPool::new(
                if selected
                    .iter()
                    .any(|endpoint| config.services[&endpoint.service].proxy.is_some())
                {
                    1
                } else {
                    ssh.max_concurrent_starts as usize
                },
            );
            let wave_one_futures = build_wave(wave_one, &BTreeMap::new());
            let wave_one_tagged = mutation_pool.execute_concurrent(wave_one_futures).await;
            let upstream_failed: BTreeMap<String, bool> = wave_one_tagged
                .iter()
                .map(|(service_name, outcomes)| {
                    let failed = !outcomes
                        .iter()
                        .all(|(_, outcome)| matches!(outcome, EndpointOutcome::Deployed { .. }));
                    (service_name.clone(), failed)
                })
                .collect();
            let wave_two_futures = build_wave(wave_two, &upstream_failed);
            let wave_two_tagged = mutation_pool.execute_concurrent(wave_two_futures).await;
            let results: Vec<Vec<(String, EndpointOutcome)>> = wave_one_tagged
                .into_iter()
                .chain(wave_two_tagged)
                .map(|(_service_name, outcomes)| outcomes)
                .collect();
            drop(restart_spinner);

            let cron_problems = crate::cron_reconcile::reconcile_after_deploy(
                &ssh, &config, &plan, &sessions, &selected, &results,
            )
            .await;

            let server_by_identity: BTreeMap<String, String> = selected
                .iter()
                .map(|endpoint| (endpoint.identity.clone(), endpoint.server.clone()))
                .collect();
            let endpoint_outcomes = results.iter().flatten().map(|(identity, outcome)| {
                (
                    identity.clone(),
                    server_by_identity
                        .get(identity)
                        .expect("every restarted identity was selected above")
                        .clone(),
                    matches!(outcome, EndpointOutcome::Deployed { .. }),
                    match outcome {
                        EndpointOutcome::Deployed { deployment_id, .. } => {
                            Some(deployment_id.clone())
                        }
                        _ => None,
                    },
                )
            });
            audit::record_endpoints_by_server(
                &sessions,
                &plan.project,
                "service_restart",
                None,
                endpoint_outcomes,
                true,
                Some(started_at.elapsed()),
            )
            .await;

            Ui::progress("Restarting", selected.len(), selected.len());
            let mut failures = 0usize;
            for outcomes in &results {
                for (identity, outcome) in outcomes {
                    match outcome {
                        EndpointOutcome::Deployed { deployment_id, .. } => {
                            Ui::result_ok(
                                &format!("{identity}:"),
                                &format!("restarted ({})", &deployment_id[..12]),
                            );
                        }
                        EndpointOutcome::Failed { error } => {
                            Ui::result_error(&format!("{identity}:"), error);
                            failures += 1;
                        }
                        EndpointOutcome::SkippedAfterSiblingFailure => {
                            Ui::result_warn(
                                &format!("{identity}:"),
                                "skipped after a sibling replica failed",
                            );
                            failures += 1;
                        }
                    }
                }
            }

            for problem in &cron_problems {
                Ui::result_error("cron:", problem);
                failures += 1;
            }

            if failures > 0 {
                anyhow::bail!("Restart failed for {failures} endpoint(s); see the summary above.");
            }

            Ok(())
        },
    )
    .await;
    close_all(&sessions).await;
    restart_result?;
    Ui::success_elapsed("Restart completed.", started_at.elapsed());
    Ok(())
}

async fn resolve_restart_image(
    session: &SshSession,
    plan: &NetworkPlan,
    endpoint: &ServiceEndpointPlan,
    service: &Service,
) -> anyhow::Result<String> {
    if let Some(image) = &service.image {
        return container_runtime::resolve_image_reference(image, None);
    }

    let replica_id = crate::placement::endpoint_replica_id(
        &plan.project,
        &endpoint.service,
        service,
        &endpoint.server,
    )?;
    crate::agent_client::catalog(session, &plan.project)
        .await?
        .into_iter()
        .find(|record| {
            record.replica_id == replica_id
                && record.state == DeploymentState::Active
                && record.health == HealthState::Healthy
        })
        .map(|record| record.image)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Service '{}' has no `image:` configured and no healthy active deployment on '{}' to restart from. Deploy it first with `jiji deploy --build`.",
                endpoint.service,
                endpoint.server
            )
        })
}

async fn close_all(sessions: &BTreeMap<String, Arc<SshSession>>) {
    for session in sessions.values() {
        session.close().await;
    }
}
