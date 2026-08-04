use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::Context;
use jiji_config::{validate_config, NamedServer, Service};
use jiji_network::{NetworkPlanner, ServiceEndpointPlan};
use jiji_ssh::{SshPool, SshSession};
use jiji_tui::Ui;

use crate::cascade::{add_cascaded_dependents, compute_service_waves, deploy_service_endpoints};
use crate::commands::deploy::select_target_endpoints;
use crate::deploy_transaction::EndpointOutcome;
use crate::lock::{LockRequest, LockScope};
use crate::{audit, container_runtime, env_resolution, proxy, registry, ssh_adapter, version_tag};

/// Zero-downtime slot cycle onto a specific, already-published image tag: builds on the same
/// `deploy_endpoint` primitive `jiji deploy`/`jiji service restart` use (candidate placement,
/// health check, VIP cutover, proxy route activation, old-slot cleanup), but unlike restart
/// (which reuses whatever is already running) the target image is always the caller-supplied
/// `--version`, resolved purely from configuration -- no build, no registry push, no per-endpoint
/// SSH round trip to discover a current image, since the target is fully determined up front.
pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    hosts: Option<&str>,
    services: Option<&str>,
    version: Option<&str>,
    host_env: bool,
    lock_options: crate::commands::lock::AutomaticLockOptions,
) -> anyhow::Result<()> {
    Ui::section("Service Rollback:");
    let started_at = std::time::Instant::now();

    let version = version.ok_or_else(|| {
        anyhow::anyhow!(
            "`jiji service rollback` requires a target version. Pass `--version <tag>` naming a previously built and pushed image tag."
        )
    })?;
    version_tag::validate_or_bail(version)?;

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
            "No `ssh:` section configured in {}. Add at least `ssh.user:` before running service rollback.",
            path.display()
        )
    })?;

    let plan = NetworkPlanner::new()
        .plan(&config)
        .context("Could not build the private network plan")?;
    if !plan.enabled {
        anyhow::bail!(
            "Private networking is disabled in configuration; `jiji service rollback` requires it. Enable `network.enabled` and run `jiji server setup`."
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
    // A rolled-back upstream's `network_mode: service:<upstream>` dependents must be rolled back
    // too in the same invocation, in the same order `jiji deploy` cascades them -- see
    // `add_cascaded_dependents`.
    add_cascaded_dependents(&config, &plan, &mut selected, &mut replica_ids)?;
    Ui::say(
        &format!(
            "Rolling back {} endpoint(s) to version '{version}': {}",
            selected.len(),
            selected
                .iter()
                .map(|e| e.identity.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        1,
    );

    // Resolved purely from configuration before any network I/O, so a bad `image:`/`build:` setup
    // (e.g. an already-tagged image with no room for `--version`) fails fast instead of after
    // reconciling the network or connecting to hosts. Computed after cascading, so a cascaded-in
    // dependent's own image gets resolved too.
    Ui::section("Resolving Images:");
    let selected_service_names: BTreeSet<String> = selected
        .iter()
        .map(|endpoint| endpoint.service.clone())
        .collect();
    let mut images: BTreeMap<String, String> = BTreeMap::new();
    for service_name in &selected_service_names {
        let service = config.services.get(service_name).expect("checked above");
        let image = resolve_rollback_image(
            &config.builder.registry,
            &config.project,
            service_name,
            service,
            version,
        )?;
        Ui::say(&format!("{service_name}: {image}"), 1);
        images.insert(service_name.clone(), image);
    }

    // Connect once, before locking: only the servers actually hosting a selected endpoint, never
    // every configured server.
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

    let rollback_result = crate::commands::lock::with_locks(
        &pool,
        &sessions,
        &config.project,
        lock_requests,
        format!(
            "jiji service rollback: {} to {version}",
            selected_service_names
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ),
        lock_options,
        || async {
            // Rollback still performs a VIP cutover, so the installed network generation must be current
            // first -- same precondition `jiji deploy`/`jiji service restart` enforce.
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
            for service_name in &selected_service_names {
                let service = config.services.get(service_name).expect("checked above");
                let merged = env_resolution::merge_environment(&shared_env, &service.environment);
                let resolved = env_resolution::resolve_environment(&merged, &loaded_env, host_env)
                    .with_context(|| {
                        format!("Could not resolve environment for service '{service_name}'")
                    })?;
                resolved_envs.insert(service_name.clone(), resolved);
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
                        error.context(format!("kamal-proxy is not ready on '{server_name}'"))
                    );
                }
            }

            let rollback_spinner =
                Ui::spinner(&format!("Rolling back {} endpoint(s)", selected.len()));
            let engine = config.builder.engine;
            let mut endpoints_by_service: BTreeMap<String, Vec<ServiceEndpointPlan>> =
                BTreeMap::new();
            for endpoint in &selected {
                endpoints_by_service
                    .entry(endpoint.service.clone())
                    .or_default()
                    .push((*endpoint).clone());
            }

            // A rolled-back upstream's cascaded dependents must be rolled back strictly after it,
            // in a second dispatch wave -- see `compute_service_waves`.
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
                            let image = images.get(&service_name).expect("resolved above").clone();
                            let images_by_identity: BTreeMap<String, String> = endpoints
                                .iter()
                                .map(|endpoint| (endpoint.identity.clone(), image.clone()))
                                .collect();
                            let resolved_env = resolved_envs
                                .get(&service_name)
                                .expect("resolved above")
                                .clone();
                            let plan = plan.clone();
                            let sessions = sessions.clone();
                            let replica_ids = replica_ids.clone();
                            let project_root = project_root.clone();
                            let progress = rollback_spinner.handle();
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
                                    images_by_identity,
                                    resolved_env,
                                    project_root,
                                    engine,
                                    force_skip_proxy,
                                    endpoints,
                                    progress,
                                    presumed_failed,
                                    "Rolling back",
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
            drop(rollback_spinner);

            let server_by_identity: BTreeMap<String, String> = selected
                .iter()
                .map(|endpoint| (endpoint.identity.clone(), endpoint.server.clone()))
                .collect();
            let endpoint_outcomes = results.iter().flatten().map(|(identity, outcome)| {
                (
                    identity.clone(),
                    server_by_identity
                        .get(identity)
                        .expect("every rolled-back identity was selected above")
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
                "service_rollback",
                Some(&format!("version '{version}'")),
                endpoint_outcomes,
                true,
                Some(started_at.elapsed()),
            )
            .await;

            Ui::progress("Rolling back", selected.len(), selected.len());
            let mut failures = 0usize;
            for outcomes in &results {
                for (identity, outcome) in outcomes {
                    match outcome {
                        EndpointOutcome::Deployed { deployment_id, .. } => {
                            Ui::result_ok(
                                &format!("{identity}:"),
                                &format!("rolled back to '{version}' ({})", &deployment_id[..12]),
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

            if failures > 0 {
                anyhow::bail!("Rollback failed for {failures} endpoint(s); see the summary above.");
            }

            Ok(())
        },
    )
    .await;
    close_all(&sessions).await;
    rollback_result?;
    Ui::success_elapsed("Rollback completed.", started_at.elapsed());
    Ok(())
}

/// Resolves the exact image reference for `--version` without touching the builder or registry --
/// a build-configured service's versioned tag is fully determined by `builder.registry` + project +
/// service name (the same reference `jiji build`/`jiji deploy --build` would have already pushed),
/// and a static `image:` service just gets `--version` applied the same way `jiji deploy --version`
/// does. Trusts that the requested tag was already published; if it wasn't, the candidate container
/// fails to start/pull and is reported as a normal deploy failure below, not a rollback-specific one.
fn resolve_rollback_image(
    registry_config: &jiji_config::Registry,
    project: &str,
    service_name: &str,
    service: &Service,
    version: &str,
) -> anyhow::Result<String> {
    if service.build.is_some() {
        return registry::full_image_name(registry_config, project, service_name, version).map_err(
            |error| {
                anyhow::anyhow!(
                    "Could not resolve rollback image for service '{service_name}': {error}"
                )
            },
        );
    }
    let image = service.image.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "Service '{service_name}' has no `image:` or `build:` configured. Set one before rolling back."
        )
    })?;
    container_runtime::resolve_image_reference(image, Some(version)).map_err(|error| {
        anyhow::anyhow!("Could not resolve rollback image for service '{service_name}': {error}")
    })
}

async fn close_all(sessions: &BTreeMap<String, Arc<SshSession>>) {
    for session in sessions.values() {
        session.close().await;
    }
}
