use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::Context;
use jiji_config::{load_config, validate_config, NamedServer, Service};
use jiji_network::{NetworkPlanner, ServiceEndpointPlan};
use jiji_ssh::{SshPool, SshSession};
use jiji_tui::Ui;

use crate::commands::deploy::{select_target_endpoints, DEFAULT_MAX_DIR_UPLOAD_BYTES};
use crate::deploy_transaction::{deploy_endpoint, EndpointDeploymentContext, EndpointOutcome};
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

    let selected = select_target_endpoints(&plan, hosts, services)?;
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
    // reconciling the network or connecting to hosts.
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

    // Rollback still performs a VIP cutover, so the installed network generation must be current
    // first -- same precondition `jiji deploy`/`jiji service restart` enforce.
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
    for service_name in &selected_service_names {
        let service = config.services.get(service_name).expect("checked above");
        let merged = env_resolution::merge_environment(&shared_env, &service.environment);
        let resolved = env_resolution::resolve_environment(&merged, &loaded_env, host_env)
            .with_context(|| {
                format!("Could not resolve environment for service '{service_name}'")
            })?;
        resolved_envs.insert(service_name.clone(), resolved);
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

    Ui::section("Rolling Back:");
    let engine = config.builder.engine;
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
                let image = images.get(&service_name).expect("resolved above");
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
                .expect("every rolled-back identity was selected above")
                .clone(),
            matches!(outcome, EndpointOutcome::Deployed { .. }),
        )
    });
    audit::record_endpoints_by_server(
        &sessions,
        &plan.project,
        "service_rollback",
        Some(&format!("version '{version}'")),
        endpoint_outcomes,
        Some(started_at.elapsed()),
    )
    .await;
    close_all(&sessions).await;

    Ui::section("Rollback Summary:");
    let mut failures = 0usize;
    for outcomes in &results {
        for (identity, outcome) in outcomes {
            match outcome {
                EndpointOutcome::Deployed { candidate_slot } => {
                    Ui::say(
                        &format!("{identity}: rolled back to '{version}' (slot {candidate_slot})"),
                        1,
                    );
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
        anyhow::bail!("Rollback failed for {failures} endpoint(s); see the summary above.");
    }

    Ui::success("\nRollback completed.");
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
