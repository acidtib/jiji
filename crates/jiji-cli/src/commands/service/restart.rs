use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::Context;
use jiji_config::{load_config, validate_config, ContainerEngine, NamedServer, Service};
use jiji_network::{NetworkPlan, NetworkPlanner, ServiceEndpointPlan};
use jiji_ssh::{SshPool, SshSession};
use jiji_tui::Ui;

use crate::commands::deploy::{select_target_endpoints, DEFAULT_MAX_DIR_UPLOAD_BYTES};
use crate::deploy_transaction::{deploy_endpoint, EndpointDeploymentContext, EndpointOutcome};
use crate::{
    audit, container_ops, container_runtime, env_resolution, proxy, service_network, ssh_adapter,
};

/// Zero-downtime slot cycle: builds on the exact same `deploy_endpoint` primitive `jiji deploy`
/// uses (candidate placement, health check, VIP cutover, proxy route activation, old-slot
/// cleanup), reusing whatever image is already configured/running rather than building or
/// bumping a version -- restart's whole point is to cycle the container in place.
pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    hosts: Option<&str>,
    services: Option<&str>,
    host_env: bool,
) -> anyhow::Result<()> {
    Ui::section("Service Restart:");

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

    let selected = select_target_endpoints(&plan, hosts, services)?;
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

    // Restart still performs a VIP cutover, so the installed network generation must be current
    // first -- same precondition `jiji deploy` enforces.
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

    let shared_env = config.environment.clone().unwrap_or_default();
    let mut resolved_envs: BTreeMap<String, env_resolution::ResolvedEnvironment> = BTreeMap::new();
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
            proxy_address: server_plan.proxy_address,
        });
        if let Err(error) =
            proxy::ensure_proxy(session, config.builder.engine, network, false).await
        {
            close_all(&sessions).await;
            return Err(error.context(format!("kamal-proxy is not ready on '{server_name}'")));
        }
    }

    // Resolved per endpoint identity, not per service: a build-only service's currently-running
    // image is discovered by inspecting that specific replica's active container, so different
    // replicas of the same service could in principle be restarted from different images if a
    // prior deploy only reached some of them. Run concurrently through the same pool used for
    // connecting/restarting -- each lookup is an independent SSH round trip.
    Ui::section("Resolving Images:");
    let engine = config.builder.engine;
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
                resolve_restart_image(&session, engine, &plan_for_image, &endpoint, &service).await;
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
        close_all(&sessions).await;
        anyhow::bail!(
            "Could not resolve the restart image for {} endpoint(s); see the errors above.",
            image_failures.len()
        );
    }

    Ui::section("Restarting:");
    let mut endpoints_by_service: BTreeMap<String, Vec<ServiceEndpointPlan>> = BTreeMap::new();
    for endpoint in &selected {
        endpoints_by_service
            .entry(endpoint.service.clone())
            .or_default()
            .push((*endpoint).clone());
    }

    let mut service_futures = Vec::new();
    for (service_name, endpoints) in endpoints_by_service {
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
        let project_root = project_root.clone();

        service_futures.push(move || async move {
            let mut outcomes = Vec::new();
            let mut sibling_failed = false;
            for endpoint in &endpoints {
                if sibling_failed {
                    outcomes.push((
                        endpoint.identity.clone(),
                        EndpointOutcome::SkippedAfterSiblingFailure,
                    ));
                    continue;
                }
                let session = sessions.get(&endpoint.server).expect("connected above");
                let server = &plan.servers[&endpoint.server];
                let image = images.get(&endpoint.identity).expect("resolved above");
                let ctx = EndpointDeploymentContext {
                    session,
                    plan: &plan,
                    server,
                    endpoint,
                    service_name: &service_name,
                    service: &service,
                    engine,
                    image,
                    resolved_env: &resolved_env,
                    project_root: &project_root,
                    skip_proxy: false,
                    max_dir_upload_bytes: DEFAULT_MAX_DIR_UPLOAD_BYTES,
                };
                let outcome = deploy_endpoint(&ctx).await;
                if !matches!(outcome, EndpointOutcome::Deployed { .. }) {
                    sibling_failed = true;
                }
                outcomes.push((endpoint.identity.clone(), outcome));
            }
            outcomes
        });
    }

    let results = pool.execute_concurrent(service_futures).await;

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
        )
    });
    audit::record_endpoints_by_server(
        &sessions,
        &plan.project,
        "service_restart",
        None,
        endpoint_outcomes,
    )
    .await;
    close_all(&sessions).await;

    Ui::section("Restart Summary:");
    let mut failures = 0usize;
    for outcomes in &results {
        for (identity, outcome) in outcomes {
            match outcome {
                EndpointOutcome::Deployed { candidate_slot } => {
                    Ui::say(&format!("{identity}: restarted (slot {candidate_slot})"), 1);
                }
                EndpointOutcome::Failed { error } => {
                    Ui::error(&format!("{identity}: {error}"));
                    failures += 1;
                }
                EndpointOutcome::SkippedAfterSiblingFailure => {
                    Ui::warn(&format!(
                        "{identity}: skipped after a sibling replica failed"
                    ));
                    failures += 1;
                }
            }
        }
    }

    if failures > 0 {
        anyhow::bail!("Restart failed for {failures} endpoint(s); see the summary above.");
    }

    Ui::success("\nRestart completed.");
    Ok(())
}

async fn resolve_restart_image(
    session: &SshSession,
    engine: ContainerEngine,
    plan: &NetworkPlan,
    endpoint: &ServiceEndpointPlan,
    service: &Service,
) -> anyhow::Result<String> {
    if let Some(image) = &service.image {
        return container_runtime::resolve_image_reference(image, None);
    }

    let active_slot = service_network::load_active_slots(session, plan)
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?
        .active_slot(&endpoint.identity);
    let Some(slot) = active_slot else {
        anyhow::bail!(
            "Service '{}' has no `image:` configured and no running container on '{}' to restart from. Deploy it first with `jiji deploy --build`.",
            endpoint.service,
            endpoint.server
        );
    };
    let container = container_runtime::container_name(&plan.project, &endpoint.service, slot);
    container_ops::inspect_image_ref(session, engine, &container)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Service '{}' is marked active on '{}' but its container '{container}' could not be inspected (missing, or the container engine could not be reached). Repair it manually (or reconcile the VIP mapping) before retrying `jiji service restart`.",
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
