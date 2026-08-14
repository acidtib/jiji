use std::collections::{BTreeMap, BTreeSet};
use std::io::IsTerminal;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use jiji_config::{validate_config, NamedServer};
use jiji_network::{NetworkPlan, NetworkPlanner, ServiceEndpointPlan};
use jiji_ssh::{RemoteForward, SshPool, SshSession};
use jiji_tui::Ui;

use crate::audit;
use crate::cascade::{add_cascaded_dependents, compute_service_waves, deploy_service_endpoints};
use crate::deploy_transaction::EndpointOutcome;
use crate::lock::{LockRequest, LockScope};
use crate::{
    build_engine, build_executor, build_plan, container_runtime, engine, env_resolution, proxy,
    proxy_routes, registry, ssh_adapter, version_tag,
};

pub(crate) const DEFAULT_MAX_DIR_UPLOAD_BYTES: u64 = 100 * 1024 * 1024;

#[allow(clippy::too_many_arguments)]
pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    hosts: Option<&str>,
    services: Option<&str>,
    version: Option<&str>,
    build: bool,
    no_cache: bool,
    skip_proxy: bool,
    host_env: bool,
    yes: bool,
    lock_timeout: u64,
    force_lock: bool,
    wait_for_peers: Option<u32>,
) -> anyhow::Result<()> {
    Ui::section("Deploy:");
    let started_at = std::time::Instant::now();

    if no_cache && !build {
        Ui::warn("--no-cache has no effect without --build");
    }

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
            "No `ssh:` section configured in {}. Add at least `ssh.user:` before running deploy.",
            path.display()
        )
    })?;

    // Resolved (and, for a remote target, validated against `ssh:`) before network planning or
    // any SSH connection -- a broken `builder.remote` fails here, before the confirmation
    // prompt, rather than mid-build.
    let executor_target = build_plan::select_executor(&config)?;

    let plan = NetworkPlanner::new()
        .plan(&config)
        .context("Could not build the private network plan")?;
    if !plan.enabled {
        anyhow::bail!(
            "Private networking is disabled in configuration; `jiji deploy` requires it. Enable `network.enabled` and run `jiji server setup`."
        );
    }

    let (configured_selected, _) = select_replica_endpoints(&config, &plan, hosts, services)?;
    let selected_services = configured_selected
        .iter()
        .map(|endpoint| endpoint.service.as_str())
        .collect::<BTreeSet<_>>();
    let seed_server = selected_services
        .iter()
        .flat_map(|service_name| config.services[*service_name].servers.iter())
        .min()
        .expect("selected services have eligible servers");
    let seed_config = &config.servers[seed_server];
    let seed_options = ssh_adapter::connect_options(seed_server, seed_config, &ssh)?;
    let seed_session = Arc::new(
        SshSession::connect(&seed_options)
            .await
            .with_context(|| format!("Could not connect to seed server '{seed_server}'"))?,
    );
    // Checked before the first real agent RPC (reading desired placement, right below): an
    // uninstalled/pre-agent host otherwise fails that raw remote command instead of surfacing
    // the actionable "run `jiji server setup`" hint `check_version` gives.
    crate::agent_client::check_version(&seed_session, &config.project, seed_server).await?;
    let seed_sessions = BTreeMap::from([(seed_server.clone(), Arc::clone(&seed_session))]);
    let (mut selected, mut replica_ids, ingress_hosts) = select_effective_replica_endpoints(
        &config,
        &plan,
        hosts,
        &selected_services,
        &seed_sessions,
    )
    .await?;
    add_cascaded_dependents(&config, &plan, &mut selected, &mut replica_ids)?;
    confirm_deployment_plan(
        &config.project,
        environment,
        &selected,
        build,
        build.then(|| executor_target.identity()).as_deref(),
        version,
        skip_proxy,
        yes,
    )?;

    let server_names: BTreeSet<String> = selected
        .iter()
        .map(|e| e.server.clone())
        .chain(ingress_hosts.iter().cloned())
        .collect();
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

    // Connect once for the whole command: the seed session opened above is merged into this set
    // instead of being closed and reopened, and every other needed host is connected exactly once
    // here -- reused for lock acquire/release, the deploy work itself, and the closing audit write.
    Ui::section("Connecting:");
    let mut sessions: BTreeMap<String, Arc<SshSession>> = BTreeMap::new();
    if server_names.contains(seed_server) {
        Ui::say(
            &format!("{seed_server} ({}): connected", seed_config.host),
            1,
        );
        sessions.insert(seed_server.clone(), seed_session);
    } else {
        seed_session.close().await;
    }
    let remaining: Vec<(String, NamedServer)> = named_servers
        .iter()
        .filter(|(name, _)| !sessions.contains_key(name))
        .cloned()
        .collect();
    let operations: Vec<_> = remaining
        .iter()
        .map(|(name, _)| connect_options.get(name).expect("inserted above").clone())
        .map(|options| move || async move { SshSession::connect(&options).await })
        .collect();
    let connections = pool.execute_concurrent(operations).await;
    let mut connection_failures = Vec::new();
    for ((name, server), connection) in remaining.iter().zip(connections) {
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

    let mut lock_requests: Vec<LockRequest> = selected
        .iter()
        .map(|endpoint| {
            let replica_id = replica_ids
                .get(&endpoint.identity)
                .expect("selected replica has an identity")
                .clone();
            LockRequest::new(
                LockScope::LogicalReplica { replica_id },
                endpoint.server.clone(),
            )
        })
        .collect();
    if !skip_proxy {
        for host in &ingress_hosts {
            lock_requests.push(LockRequest::new(LockScope::HostGlobalProxy, host.clone()));
        }
    }

    let deploy_result = crate::commands::lock::with_locks(
        &pool,
        &sessions,
        &config.project,
        lock_requests,
        format!(
            "jiji deploy: {}",
            selected_service_names_for_message(&selected)
        ),
        crate::commands::lock::AutomaticLockOptions {
            timeout: lock_timeout,
            force: force_lock,
        },
        || async {
    crate::commands::network::setup::reconcile_for_deploy(&config, &plan).await?;

    let project_root = env_resolution::project_root_from_config_path(&path);
    let (loaded_env, loaded_from) =
        env_resolution::load_env_file(&project_root, environment, config.secrets_path.as_deref())?;
    if let Some(loaded_from) = &loaded_from {
        Ui::say(
            &format!("Environment loaded from: {}", loaded_from.display()),
            1,
        );
    }

    let mut images: BTreeMap<String, String> = BTreeMap::new();
    let selected_service_names: BTreeSet<String> = selected
        .iter()
        .map(|endpoint| endpoint.service.clone())
        .collect();
    let services_to_build: BTreeSet<String> = selected_service_names
        .iter()
        .filter(|name| {
            build
                && config
                    .services
                    .get(*name)
                    .is_some_and(|service| service.build.is_some())
        })
        .cloned()
        .collect();
    let mut registry_password = None;
    if build && services_to_build.is_empty() {
        Ui::warn("--build was passed, but no selected service has `build:` configured");
    }
    if !services_to_build.is_empty() {
        let is_remote_executor =
            matches!(executor_target, build_plan::ExecutorTarget::Remote { .. });
        let executor_identity = executor_target.identity();
        let git = version_tag::gather_git_status().await;
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let (build_version, warning) = version_tag::resolve_version_tag(version, git.as_ref(), now);
        if let Some(warning) = warning {
            Ui::warn(&warning);
        }
        version_tag::validate_or_bail(&build_version)?;
        let build_services: Vec<String> = services_to_build.iter().cloned().collect();
        let mut build_plan =
            build_plan::compute_plan(&config, &config.project, &build_services, &build_version)?;
        build_plan::resolve_build_arg_references(
            &config,
            &mut build_plan,
            &loaded_env,
            host_env,
        )?;
        let resolved_secrets =
            build_plan::resolve_build_secrets(&config, &build_plan, &loaded_env, host_env)?;
        for entry in &build_plan {
            if let Some(error) = build_engine::multi_arch_requires_push(&entry.platforms, true) {
                anyhow::bail!("Service '{}': {error}", entry.service_name);
            }
        }
        if config.builder.registry.is_local() {
            Ui::section("Local Registry:");
            registry::ensure_local_registry(config.builder.engine, &config.builder.registry)
                .await?;
        } else {
            let raw_password = config.builder.registry.password.as_deref().ok_or_else(|| {
                        anyhow::anyhow!(
                            "`jiji deploy --build` requires `builder.registry.password` so deploy hosts can pull the built image."
                        )
                    })?;
            if config.builder.registry.username.is_none() {
                anyhow::bail!(
                    "`jiji deploy --build` requires `builder.registry.username` so deploy hosts can pull the built image."
                );
            }
            let password = registry::resolve_registry_password(raw_password, &loaded_env, host_env)
                .await?;
            registry_password = Some(password);
        }
        let mut executor = build_executor::BuildExecutor::prepare(
            executor_target,
            config.builder.engine,
            &config.project,
            &build_plan,
        )
        .await?;
        if let Some(engine::EngineStatus::Installed(version)) = executor.engine_status() {
            Ui::say(
                &format!(
                    "{} {version} installed on {executor_identity}",
                    config.builder.engine
                ),
                1,
            );
        }
        if let Some(engine::EngineStatus::Upgraded { from, to }) = executor.engine_status() {
            Ui::say(
                &format!(
                    "{} upgraded from {from} to {to} on {executor_identity}",
                    config.builder.engine
                ),
                1,
            );
        }

        // Everything from here that can fail must still let `executor.finish()` run (staging
        // cleanup, tunnel cancellation, session close) -- so every failure is captured into
        // `run_result` instead of an early `?` return, and combined with the cleanup outcome
        // once, at the end, regardless of where in this sequence things went wrong.
        Ui::section("Registry:");
        let mut run_result = executor
            .prepare_registry(
                config.builder.engine,
                &config.builder.registry,
                registry_password.as_deref(),
            )
            .await;
        if run_result.is_ok() && is_remote_executor {
            if config.builder.registry.is_local() {
                Ui::say(
                    &format!("Tunneled local registry to {executor_identity}"),
                    1,
                )
            } else {
                Ui::say(&format!("Logged in on {executor_identity}"), 1)
            }
        }

        if run_result.is_ok() {
            Ui::section("Building:");
            for entry in &build_plan {
                Ui::say(&entry.service_name, 1);
                run_result = build_plan::build_one(
                    entry,
                    &executor,
                    config.builder.engine,
                    no_cache,
                    true,
                    &config.project,
                    &project_root,
                    config.builder.registry.is_local(),
                    &resolved_secrets[&entry.service_name],
                )
                .await
                .with_context(|| format!("Build failed for service '{}'", entry.service_name));
                if run_result.is_err() {
                    break;
                }
                images.insert(entry.service_name.clone(), entry.version_ref.clone());
            }
        }
        build_executor::combine_with_cleanup_error(run_result, executor.finish().await)?;
    }

    for endpoint in &selected {
        if images.contains_key(&endpoint.service) {
            continue;
        }
        let service = config.services.get(&endpoint.service).ok_or_else(|| {
            anyhow::anyhow!(
                "Service '{}' is not defined in configuration",
                endpoint.service
            )
        })?;
        let image = service.image.as_deref().ok_or_else(|| match &service.build {
            Some(_) => anyhow::anyhow!(
                "Service '{}' has no `image:` configured, but has `build:` configured. Pass `--build` to build and push it, or set `image:` directly.",
                endpoint.service
            ),
            None => anyhow::anyhow!(
                "Service '{}' has no `image:` configured. Set an image reference before deploying.",
                endpoint.service
            ),
        })?;
        let resolved =
            container_runtime::resolve_image_reference(image, version).with_context(|| {
                format!("Could not resolve image for service '{}'", endpoint.service)
            })?;
        images.insert(endpoint.service.clone(), resolved);
    }

    let shared_env = config.environment.clone().unwrap_or_default();
    let mut resolved_envs: BTreeMap<String, env_resolution::ResolvedEnvironment> = BTreeMap::new();
    for endpoint in &selected {
        if resolved_envs.contains_key(&endpoint.service) {
            continue;
        }
        let service = config
            .services
            .get(&endpoint.service)
            .expect("checked above");
        let mut merged = env_resolution::merge_environment(&shared_env, &service.environment);
        proxy_routes::add_tls_secret_refs(service.proxy.as_ref(), &mut merged);
        let mut resolved = env_resolution::resolve_environment(&merged, &loaded_env, host_env)
            .with_context(|| {
                format!(
                    "Could not resolve environment for service '{}'",
                    endpoint.service
                )
            })?;
        proxy_routes::mark_tls_control_secrets(service.proxy.as_ref(), &mut resolved);
        tracing::debug!(
            "{}: environment: {}",
            endpoint.service,
            env_resolution::redacted_summary(&resolved).join(", ")
        );
        resolved_envs.insert(endpoint.service.clone(), resolved);
    }

    let mut registry_forwards: Vec<(String, RemoteForward)> = Vec::new();
    if !services_to_build.is_empty() && config.builder.registry.is_local() {
        Ui::section("Registry Tunnels:");
        let hosts = hosts_serving_build_configured_services(&selected, &services_to_build);
        for server_name in hosts {
            let session = sessions.get(&server_name).expect("connected above");
            match session
                .start_reverse_forward(
                    "127.0.0.1",
                    config.builder.registry.port,
                    config.builder.registry.port,
                )
                .await
            {
                Ok(forward) => {
                    registry_forwards.push((server_name.clone(), forward));
                    Ui::say(
                        &format!(
                            "{server_name}: remote localhost:{} -> local registry",
                            config.builder.registry.port
                        ),
                        1,
                    );
                }
                Err(error) => {
                    cancel_forwards(&sessions, &registry_forwards).await;
                    return Err(anyhow::Error::new(error).context(format!(
                        "Could not expose the local registry to deploy host '{server_name}'"
                    )));
                }
            }
        }
    }

    if let Some(password) = registry_password.as_deref() {
        Ui::section("Registry Login:");
        let hosts = hosts_serving_build_configured_services(&selected, &services_to_build);
        for server_name in hosts {
            let session = sessions.get(&server_name).expect("connected above");
            if let Err(error) = registry::login_remote(
                session,
                config.builder.engine,
                &config.builder.registry,
                password,
            )
            .await
            {
                cancel_forwards(&sessions, &registry_forwards).await;
                return Err(error.context(format!(
                    "Registry login failed on deploy host '{server_name}'"
                )));
            }
            Ui::say(&format!("{server_name}: authenticated"), 1);
        }
    }

    if !services_to_build.is_empty() {
        Ui::section("Pulling Built Images:");
        let mut pulled = BTreeSet::new();
        for endpoint in &selected {
            if !services_to_build.contains(&endpoint.service)
                || !pulled.insert((endpoint.server.clone(), endpoint.service.clone()))
            {
                continue;
            }
            let session = sessions.get(&endpoint.server).expect("connected above");
            let image = images.get(&endpoint.service).expect("built above");
            let spinner = Ui::spinner(&format!("{}: pulling {image}", endpoint.server));
            let spinner_handle = spinner.handle();
            let server_name = endpoint.server.clone();
            let pull_result = crate::container_ops::pull_image_with_progress(
                session,
                config.builder.engine,
                image,
                |status| spinner_handle.set_message(&format!("{server_name}: {status}")),
            )
            .await;
            drop(spinner);
            if let Err(error) = pull_result {
                cancel_forwards(&sessions, &registry_forwards).await;
                return Err(error.context(format!(
                    "Could not pull newly built image for service '{}' on deploy host '{}'",
                    endpoint.service, endpoint.server
                )));
            }
            Ui::say(&format!("{}: {image}", endpoint.server), 1);
        }
    }

    Ui::section("Verifying Agent:");
    for (server_name, session) in &sessions {
        if let Err(error) =
            crate::agent_client::check_version(session, &config.project, server_name).await
        {
            cancel_forwards(&sessions, &registry_forwards).await;
            return Err(error);
        }
        Ui::say(&format!("{server_name}: ready"), 1);
    }

    if !skip_proxy {
        Ui::section("Verifying Proxy:");
        for (server_name, session) in &sessions {
            let serves_proxy = ingress_hosts.contains(server_name);
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
                cancel_forwards(&sessions, &registry_forwards).await;
                return Err(error.context(format!("jiji-proxy is not ready on '{server_name}'")));
            }
            Ui::say(&format!("{server_name}: ready"), 1);
        }
    }

    Ui::section("Deploying:");
    let endpoint_identities: Vec<(String, String)> = selected
        .iter()
        .map(|e| (e.identity.clone(), e.server.clone()))
        .collect();
    let deploy_progress = Ui::deploy_progress_with_servers(endpoint_identities);
    let deploy_handle = deploy_progress.handle();
    let mut endpoints_by_service: BTreeMap<String, Vec<ServiceEndpointPlan>> = BTreeMap::new();
    for endpoint in &selected {
        endpoints_by_service
            .entry(endpoint.service.clone())
            .or_default()
            .push((*endpoint).clone());
    }

    // A `network_mode: service:<upstream>` dependent whose upstream is *also* selected this run
    // must not be deployed until the upstream's own deploy has fully finished (its old container
    // isn't removed, and its new one doesn't exist, until then) -- see `add_cascaded_dependents`
    // above and `compute_service_waves` for why this needs two sequential dispatch waves rather
    // than an in-closure wait.
    let (dependents_of, wave_one, wave_two) = compute_service_waves(&config, endpoints_by_service);

    let build_wave = |endpoints_by_service: BTreeMap<String, Vec<ServiceEndpointPlan>>,
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
                let engine = config.builder.engine;
                let project_root = project_root.clone();
                let progress = Some(deploy_handle.clone());
                // An upstream with a dependent in the second wave must not activate its own proxy
                // route inline: that happens before this function returns, well before the
                // dependent (whose port the route may target) has redeployed. Deferred instead to
                // the `reconcile_catalog_routes` pass below, which already runs once after every
                // selected endpoint -- upstream and cascaded dependents alike -- has finished.
                let force_skip_proxy = skip_proxy || dependents_of.contains_key(&service_name);
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
                    )
                }
            })
            .collect::<Vec<_>>()
    };

    let mutation_pool = SshPool::new(if selected.iter().any(|endpoint| {
        config.services[&endpoint.service].proxy.is_some()
    }) {
        1
    } else {
        ssh.max_concurrent_starts as usize
    });
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
    deploy_progress.finish();
    cancel_forwards(&sessions, &registry_forwards).await;
    // Runs regardless of `skip_proxy`/ingress outcome below: a service's own endpoints already
    // succeeded or failed by this point, and cron reconciliation is independent of proxy routing.
    // Never fails the deploy itself (Phase 5's "partial failure" requirement) -- problems are
    // folded into the failure count alongside endpoint/ingress failures further down.
    let cron_problems = crate::cron_reconcile::reconcile_after_deploy(
        &ssh, &config, &plan, &sessions, &selected, &results,
    )
    .await;
    let image_retention_problems = crate::image_retention_reconcile::reconcile_after_deploy(
        &ssh, &config, &selected, &results, &sessions,
    )
    .await;
    let deployment_succeeded = results
        .iter()
        .flatten()
        .all(|(_, outcome)| matches!(outcome, EndpointOutcome::Deployed { .. }));
    let ingress_error = if deployment_succeeded && !skip_proxy {
        let proxy_services = selected
            .iter()
            .filter_map(|endpoint| {
                config.services[&endpoint.service]
                    .proxy
                    .clone()
                    .map(|proxy| (endpoint.service.clone(), proxy))
            })
            .collect::<BTreeMap<_, _>>();
        let dns_servers: BTreeMap<String, std::net::SocketAddr> = sessions
            .keys()
            .map(|server| {
                (
                    server.clone(),
                    std::net::SocketAddr::new(plan.servers[server].dns_address.into(), 53),
                )
            })
            .collect();
        proxy_routes::reconcile_catalog_routes(
            &sessions,
            &dns_servers,
            &plan.project,
            config.builder.engine,
            &proxy_services,
            &resolved_envs,
        )
        .await
        .err()
    } else {
        None
    };

    // One audit entry per server, summarizing every endpoint deployed on it during this run --
    // reuses the same session set the deploy itself just used; the caller closes it once, after
    // this closure returns and the lock has been released.
    let server_by_identity: BTreeMap<String, String> = selected
        .iter()
        .map(|endpoint| (endpoint.identity.clone(), endpoint.server.clone()))
        .collect();
    let endpoint_outcomes = results.iter().flatten().map(|(identity, outcome)| {
        (
            identity.clone(),
            server_by_identity
                .get(identity)
                .expect("every deployed identity was selected above")
                .clone(),
            matches!(outcome, EndpointOutcome::Deployed { .. }),
            match outcome {
                EndpointOutcome::Deployed { deployment_id, .. } => Some(deployment_id.clone()),
                _ => None,
            },
        )
    });
    audit::record_endpoints_by_server(
        &sessions,
        &plan.project,
        "deploy",
        None,
        endpoint_outcomes,
        true,
        Some(started_at.elapsed()),
    )
    .await;

    Ui::progress("Deploying", selected.len(), selected.len());
    let mut failures = 0usize;
    for outcomes in &results {
        for (identity, outcome) in outcomes {
            match outcome {
                EndpointOutcome::Deployed { deployment_id, .. } => {
                    Ui::result_ok(
                        &format!("{identity}:"),
                        &format!("deployed ({})", &deployment_id[..12]),
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
    if let Some(error) = ingress_error {
        Ui::result_error("ingress:", &error.to_string());
        failures += 1;
    }
    for problem in &cron_problems {
        Ui::result_error("cron:", problem);
        failures += 1;
    }
    for problem in &image_retention_problems {
        Ui::result_error("image-retention:", problem);
        failures += 1;
    }

    if failures > 0 {
        anyhow::bail!("Deploy failed for {failures} endpoint(s); see the summary above.");
    }

    if let Some(wait_for_peers) = wait_for_peers {
        let deployment_ids: BTreeSet<String> = results
            .iter()
            .flatten()
            .filter_map(|(_, outcome)| match outcome {
                EndpointOutcome::Deployed { deployment_id, .. } => Some(deployment_id.clone()),
                _ => None,
            })
            .collect();
        report_peer_replication_ack(
            &ssh,
            &config.servers,
            &sessions,
            &plan.project,
            &deployment_ids,
            wait_for_peers,
        )
        .await;
    }

    Ok(())
        },
    )
    .await;
    close_all(&sessions).await;
    deploy_result?;
    Ui::success_elapsed("Deployment completed.", started_at.elapsed());
    Ok(())
}

fn selected_service_names_for_message(selected: &[ServiceEndpointPlan]) -> String {
    selected
        .iter()
        .map(|endpoint| endpoint.service.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(", ")
}

/// Selects endpoints matching both `--hosts` and `--services`. Each filter alone reuses
/// `NetworkPlan::select_hosts`/`select_endpoints` (already unit-tested, already raising an
/// actionable `NetworkPlanError` on an unmatched pattern); an empty intersection between the two
/// filters is a new, explicit error since neither primitive alone can detect it.
pub(crate) fn select_target_endpoints<'a>(
    plan: &'a NetworkPlan,
    hosts: Option<&str>,
    services: Option<&str>,
) -> anyhow::Result<Vec<&'a ServiceEndpointPlan>> {
    let host_filters = split_comma_trimmed(hosts);
    let service_filters = split_comma_trimmed(services);

    let target_hosts: BTreeSet<String> = plan
        .select_hosts(&host_filters)?
        .into_iter()
        .map(|server| server.name.clone())
        .collect();
    let candidates = plan.select_endpoints(&service_filters)?;
    let selected: Vec<&ServiceEndpointPlan> = candidates
        .into_iter()
        .filter(|endpoint| target_hosts.contains(&endpoint.server))
        .collect();

    if selected.is_empty() {
        anyhow::bail!("No service endpoint matches both --hosts and --services filters");
    }
    Ok(selected)
}

fn select_replica_endpoints(
    config: &jiji_config::Config,
    plan: &NetworkPlan,
    hosts: Option<&str>,
    services: Option<&str>,
) -> anyhow::Result<(Vec<ServiceEndpointPlan>, BTreeMap<String, String>)> {
    let eligible = select_target_endpoints(plan, hosts, services)?;
    let allowed_hosts = eligible
        .iter()
        .map(|endpoint| endpoint.server.as_str())
        .collect::<BTreeSet<_>>();
    let service_names = eligible
        .iter()
        .map(|endpoint| endpoint.service.as_str())
        .collect::<BTreeSet<_>>();
    let mut selected = Vec::new();
    let mut replica_ids = BTreeMap::new();
    for service_name in service_names {
        let service = &config.services[service_name];
        for assignment in crate::placement::place(
            &config.project,
            service_name,
            service.replicas,
            &service.servers,
            service.placement,
        ) {
            if !allowed_hosts.contains(assignment.server.as_str()) {
                continue;
            }
            let mut endpoint = plan
                .endpoints
                .values()
                .find(|endpoint| {
                    endpoint.service == service_name && endpoint.server == assignment.server
                })
                .cloned()
                .expect("placement uses a configured service endpoint");
            endpoint.identity = format!(
                "{}:{}:{}",
                config.project, service_name, assignment.replica_id
            );
            replica_ids.insert(endpoint.identity.clone(), assignment.replica_id);
            selected.push(endpoint);
        }
    }
    if selected.is_empty() {
        anyhow::bail!(
            "No desired service replica matches both --hosts and --services filters; use `jiji service scale` to change placement"
        );
    }
    selected.sort_by(|a, b| a.identity.cmp(&b.identity));
    Ok((selected, replica_ids))
}

async fn select_effective_replica_endpoints(
    config: &jiji_config::Config,
    plan: &NetworkPlan,
    hosts: Option<&str>,
    service_names: &BTreeSet<&str>,
    sessions: &BTreeMap<String, Arc<SshSession>>,
) -> anyhow::Result<(
    Vec<ServiceEndpointPlan>,
    BTreeMap<String, String>,
    BTreeSet<String>,
)> {
    let allowed_hosts = plan
        .select_hosts(&split_comma_trimmed(hosts))?
        .into_iter()
        .map(|server| server.name.as_str())
        .collect::<BTreeSet<_>>();
    let seed = sessions
        .values()
        .next()
        .ok_or_else(|| anyhow::anyhow!("No connected agent is available to read desired state"))?;
    let mut selected = Vec::new();
    let mut replica_ids = BTreeMap::new();
    let mut ingress_hosts = BTreeSet::new();
    for service_name in service_names {
        let configured = &config.services[*service_name];
        if configured.proxy.is_some() {
            ingress_hosts.extend(configured.servers.iter().cloned());
        }
        let assignments = match crate::agent_client::call(
            seed,
            &config.project,
            None,
            jiji_agent::api::RequestBody::DesiredRead {
                service: (*service_name).to_string(),
            },
        )
        .await?
        {
            jiji_agent::api::ResponseBody::DesiredState {
                record: Some(record),
            } => record
                .assignments
                .into_iter()
                .map(|assignment| crate::placement::ReplicaAssignment {
                    replica_id: assignment.replica_id,
                    ordinal: assignment.ordinal,
                    server: assignment.owner_node_id,
                })
                .collect(),
            jiji_agent::api::ResponseBody::DesiredState { record: None } => {
                crate::placement::place(
                    &config.project,
                    service_name,
                    configured.replicas,
                    &configured.servers,
                    configured.placement,
                )
            }
            response => {
                anyhow::bail!("Agent returned unexpected desired-state response: {response:?}")
            }
        };
        for assignment in assignments {
            if !configured.servers.contains(&assignment.server) {
                anyhow::bail!(
                    "Desired placement for '{}' assigns '{}' to ineligible server '{}'; reset or repair it with `jiji service scale`",
                    service_name,
                    assignment.replica_id,
                    assignment.server
                );
            }
            if !allowed_hosts.contains(assignment.server.as_str()) {
                continue;
            }
            let mut endpoint = plan
                .endpoints
                .values()
                .find(|endpoint| {
                    endpoint.service == **service_name && endpoint.server == assignment.server
                })
                .cloned()
                .expect("desired placement uses a configured endpoint");
            endpoint.identity = format!(
                "{}:{}:{}",
                config.project, service_name, assignment.replica_id
            );
            replica_ids.insert(endpoint.identity.clone(), assignment.replica_id);
            selected.push(endpoint);
        }
    }
    if selected.is_empty() {
        anyhow::bail!("No effective desired replicas match the requested filters");
    }
    selected.sort_by(|a, b| a.identity.cmp(&b.identity));
    Ok((selected, replica_ids, ingress_hosts))
}

/// Prints the deployment plan and gates on confirmation before anything (build, network
/// reconciliation, SSH) happens. `--yes` skips the prompt outright; without it, a missing
/// terminal is a hard error rather than a hang, since `dialoguer::Confirm::interact` can't be
/// answered non-interactively (e.g. CI/CD, where `--yes` must be passed explicitly).
#[allow(clippy::too_many_arguments)]
fn confirm_deployment_plan(
    project: &str,
    environment: Option<&str>,
    selected: &[ServiceEndpointPlan],
    build: bool,
    executor_identity: Option<&str>,
    version: Option<&str>,
    skip_proxy: bool,
    yes: bool,
) -> anyhow::Result<()> {
    let servers: BTreeSet<&str> = selected.iter().map(|e| e.server.as_str()).collect();

    Ui::section("Deployment Plan:");
    Ui::say(&format!("Project: {project}"), 1);
    Ui::say(
        &format!("Environment: {}", environment.unwrap_or("default")),
        1,
    );
    Ui::say(
        &format!(
            "Servers: {}",
            servers.iter().copied().collect::<Vec<_>>().join(", ")
        ),
        1,
    );
    Ui::say(
        &format!(
            "Build: {}",
            if build {
                "yes (--build)"
            } else {
                "no, using configured image"
            }
        ),
        1,
    );
    if let Some(executor_identity) = executor_identity {
        Ui::say(&format!("Executor: {executor_identity}"), 1);
    }
    if let Some(version) = version {
        Ui::say(&format!("Version override: {version}"), 1);
    }
    if skip_proxy {
        Ui::say("Proxy route activation: skipped (--skip-proxy)", 1);
    }
    Ui::say(&format!("Endpoints ({}):", selected.len()), 1);
    for endpoint in selected {
        Ui::say(&format!("{} @ {}", endpoint.service, endpoint.server), 2);
    }
    let summary = format!(
        "{} endpoint(s) across {} server(s)",
        selected.len(),
        servers.len()
    );
    Ui::rule(summary.len(), 1);
    Ui::say(&summary, 1);

    if yes {
        return Ok(());
    }

    if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        anyhow::bail!(
            "Refusing to prompt for confirmation without a terminal attached. Pass --yes to confirm the deployment plan when running non-interactively (e.g. CI/CD)."
        );
    }

    let confirmed = Ui::confirm("Proceed with this deployment plan?", true)?;
    if !confirmed {
        anyhow::bail!("Deployment cancelled.");
    }
    Ok(())
}

pub(crate) fn split_comma_trimmed(value: Option<&str>) -> Vec<String> {
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

async fn close_all(sessions: &BTreeMap<String, Arc<SshSession>>) {
    for session in sessions.values() {
        session.close().await;
    }
}

/// Explicitly cancels registry reverse-tunnels opened on the shared session set (best-effort:
/// a cancellation failure here must never mask whatever primary error is already being reported).
/// Unlike the old dedicated `tunnel_sessions` map, these sessions stay open and in use for the
/// rest of the command, so the tunnel must be torn down without closing the session itself.
async fn cancel_forwards(
    sessions: &BTreeMap<String, Arc<SshSession>>,
    forwards: &[(String, RemoteForward)],
) {
    for (server_name, forward) in forwards {
        if let Some(session) = sessions.get(server_name) {
            let _ = session.cancel_reverse_forward(forward).await;
        }
    }
}

/// Best-effort, bounded, opt-in observability: after a deploy has already committed and reported
/// success, checks up to `wait_for_peers` other configured servers' catalog views for the just
/// -committed deployment IDs. Never blocks past its own short deadline and never changes the
/// command's outcome -- an unreachable or slow peer is reported as "offline", not an error.
/// Reuses an already-open session where the peer coincides with one already touched by this
/// deploy; otherwise opens a short-lived side connection bounded by the same overall deadline.
async fn report_peer_replication_ack(
    ssh: &jiji_config::Ssh,
    configured_servers: &std::collections::HashMap<String, NamedServer>,
    sessions: &BTreeMap<String, Arc<SshSession>>,
    project: &str,
    deployment_ids: &BTreeSet<String>,
    wait_for_peers: u32,
) {
    const OVERALL_DEADLINE: Duration = Duration::from_secs(10);

    if deployment_ids.is_empty() {
        return;
    }
    let mut peer_names: Vec<String> = configured_servers
        .keys()
        .filter(|name| !sessions.contains_key(*name))
        .cloned()
        .collect();
    peer_names.sort();
    peer_names.truncate(wait_for_peers as usize);
    if peer_names.is_empty() {
        Ui::say("Replication ack: no other configured peers to check.", 1);
        return;
    }

    let deadline = Instant::now() + OVERALL_DEADLINE;
    let mut confirmed = Vec::new();
    let mut offline = Vec::new();
    for name in &peer_names {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            offline.push(name.clone());
            continue;
        };
        let check = async {
            let server = configured_servers.get(name)?;
            let options = ssh_adapter::connect_options(name, server, ssh).ok()?;
            let session = SshSession::connect(&options).await.ok()?;
            let records = crate::agent_client::catalog(&session, project).await.ok();
            session.close().await;
            let records = records?;
            Some(
                records
                    .iter()
                    .any(|record| deployment_ids.contains(&record.deployment_id)),
            )
        };
        match tokio::time::timeout(remaining, check).await {
            Ok(Some(true)) => confirmed.push(name.clone()),
            _ => offline.push(name.clone()),
        }
    }

    let summary = if offline.is_empty() {
        format!(
            "Replication ack: {}/{} peer(s) confirmed within {}s.",
            confirmed.len(),
            peer_names.len(),
            OVERALL_DEADLINE.as_secs()
        )
    } else {
        format!(
            "Replication ack: {}/{} peer(s) confirmed within {}s (offline/not yet observed: {}).",
            confirmed.len(),
            peer_names.len(),
            OVERALL_DEADLINE.as_secs(),
            offline.join(", ")
        )
    };
    Ui::say(&summary, 1);
}

fn hosts_serving_build_configured_services(
    selected: &[ServiceEndpointPlan],
    services_to_build: &BTreeSet<String>,
) -> BTreeSet<String> {
    selected
        .iter()
        .filter(|endpoint| services_to_build.contains(&endpoint.service))
        .map(|endpoint| endpoint.server.clone())
        .collect()
}
