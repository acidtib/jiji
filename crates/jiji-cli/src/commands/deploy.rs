use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use jiji_config::{load_config, validate_config, NamedServer, RegistryType};
use jiji_network::{NetworkPlan, NetworkPlanner, ServiceEndpointPlan};
use jiji_ssh::{SshPool, SshSession};
use jiji_tui::Ui;

use crate::deploy_transaction::{deploy_endpoint, EndpointDeploymentContext, EndpointOutcome};
use crate::{
    build_engine, build_plan, container_runtime, env_resolution, proxy, registry, ssh_adapter,
    version_tag,
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
) -> anyhow::Result<()> {
    Ui::section("Deploy:");

    if no_cache && !build {
        Ui::warn("--no-cache has no effect without --build");
    }

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
            "No `ssh:` section configured in {}. Add at least `ssh.user:` before running deploy.",
            path.display()
        )
    })?;

    let plan = NetworkPlanner::new()
        .plan(&config)
        .context("Could not build the private network plan")?;
    if !plan.enabled {
        anyhow::bail!(
            "Private networking is disabled in configuration; `jiji deploy` requires it. Enable `network.enabled` and run `jiji server setup`."
        );
    }

    let selected = select_target_endpoints(&plan, hosts, services)?;
    Ui::say(
        &format!(
            "Deploying {} endpoint(s): {}",
            selected.len(),
            selected
                .iter()
                .map(|e| e.identity.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        1,
    );

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
        build_plan::check_scope_guards(&config.builder)?;
        let git = version_tag::gather_git_status().await;
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let (build_version, warning) = version_tag::resolve_version_tag(version, git.as_ref(), now);
        if let Some(warning) = warning {
            Ui::warn(&warning);
        }
        version_tag::validate_or_bail(&build_version)?;
        let build_services: Vec<String> = services_to_build.iter().cloned().collect();
        let build_plan =
            build_plan::compute_plan(&config, &config.project, &build_services, &build_version)?;
        for entry in &build_plan {
            if let Some(error) = build_engine::multi_arch_requires_push(&entry.platforms, true) {
                anyhow::bail!("Service '{}': {error}", entry.service_name);
            }
        }
        match config.builder.registry.kind {
            RegistryType::Local => {
                Ui::section("Local Registry:");
                registry::ensure_local_registry(config.builder.engine, &config.builder.registry)
                    .await?;
            }
            RegistryType::Remote => {
                let raw_password =
                    config.builder.registry.password.as_deref().ok_or_else(|| {
                        anyhow::anyhow!(
                            "`jiji deploy --build` requires `builder.registry.password` so deploy hosts can pull the built image."
                        )
                    })?;
                if config.builder.registry.username.is_none() {
                    anyhow::bail!(
                        "`jiji deploy --build` requires `builder.registry.username` so deploy hosts can pull the built image."
                    );
                }
                let password =
                    registry::resolve_registry_password(raw_password, &loaded_env, host_env)?;
                Ui::section("Registry Login:");
                registry::login_local(config.builder.engine, &config.builder.registry, &password)
                    .await?;
                registry_password = Some(password);
            }
        }
        Ui::section("Building:");
        for entry in &build_plan {
            Ui::say(&entry.service_name, 1);
            build_plan::build_one(
                entry,
                config.builder.engine,
                no_cache,
                true,
                &config.project,
                &project_root,
                config.builder.registry.kind == RegistryType::Local,
            )
            .await
            .with_context(|| format!("Build failed for service '{}'", entry.service_name))?;
            images.insert(entry.service_name.clone(), entry.version_ref.clone());
        }
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
        let merged = env_resolution::merge_environment(&shared_env, &service.environment);
        let resolved = env_resolution::resolve_environment(&merged, &loaded_env, host_env)
            .with_context(|| {
                format!(
                    "Could not resolve environment for service '{}'",
                    endpoint.service
                )
            })?;
        tracing::debug!(
            "{}: environment: {}",
            endpoint.service,
            env_resolution::redacted_summary(&resolved).join(", ")
        );
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

    Ui::section("Checking Deployment Lock:");
    let mut locked_hosts = Vec::new();
    for (name, session) in &sessions {
        if let Some(info) = crate::lock::read_lock(session, &plan.project).await? {
            locked_hosts.push((name.clone(), info));
        }
    }
    if !locked_hosts.is_empty() {
        close_all(&sessions).await;
        let mut detail = String::new();
        for (name, info) in &locked_hosts {
            detail.push_str(&format!(
                "\n  {name}: \"{}\" by {} ({} ago)",
                info.message,
                info.acquired_by,
                crate::lock::format_age(info.age_seconds())
            ));
        }
        anyhow::bail!(
            "Deployment lock is held on the following server(s):{detail}\nCheck `jiji lock status`, and once it's safe, `jiji lock release` before retrying.",
        );
    }

    let mut tunnel_sessions: BTreeMap<String, Arc<SshSession>> = BTreeMap::new();
    if !services_to_build.is_empty() && config.builder.registry.kind == RegistryType::Local {
        Ui::section("Registry Tunnels:");
        let hosts = hosts_serving_build_configured_services(&selected, &services_to_build);
        for server_name in hosts {
            let options = connect_options.get(&server_name).expect("connected above");
            let session = match SshSession::connect(options).await {
                Ok(session) => Arc::new(session),
                Err(error) => {
                    close_all(&tunnel_sessions).await;
                    close_all(&sessions).await;
                    return Err(anyhow::Error::new(error).context(format!(
                        "Could not open a dedicated registry tunnel connection to deploy host '{server_name}'"
                    )));
                }
            };
            match session
                .start_reverse_forward(
                    "127.0.0.1",
                    config.builder.registry.port,
                    config.builder.registry.port,
                )
                .await
            {
                Ok(_) => Ui::say(
                    &format!(
                        "{server_name}: remote localhost:{} -> local registry",
                        config.builder.registry.port
                    ),
                    1,
                ),
                Err(error) => {
                    session.close().await;
                    close_all(&tunnel_sessions).await;
                    close_all(&sessions).await;
                    return Err(anyhow::Error::new(error).context(format!(
                        "Could not expose the local registry to deploy host '{server_name}'"
                    )));
                }
            }
            tunnel_sessions.insert(server_name, session);
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
                close_all(&tunnel_sessions).await;
                close_all(&sessions).await;
                return Err(error.context(format!(
                    "Registry login failed on deploy host '{server_name}'"
                )));
            }
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
            if let Err(error) =
                crate::container_ops::pull_image(session, config.builder.engine, image).await
            {
                close_all(&tunnel_sessions).await;
                close_all(&sessions).await;
                return Err(error.context(format!(
                    "Could not pull newly built image for service '{}' on deploy host '{}'",
                    endpoint.service, endpoint.server
                )));
            }
            Ui::say(&format!("{}: {image}", endpoint.server), 1);
        }
    }

    if !skip_proxy {
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
                dns_address: server_plan.dns_address,
                proxy_address: server_plan.proxy_address,
            });
            if let Err(error) =
                proxy::ensure_proxy(session, config.builder.engine, network, false).await
            {
                close_all(&tunnel_sessions).await;
                close_all(&sessions).await;
                return Err(error.context(format!("kamal-proxy is not ready on '{server_name}'")));
            }
        }
    }

    Ui::section("Deploying:");
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
        let image = images.get(&service_name).expect("resolved above").clone();
        let resolved_env = resolved_envs
            .get(&service_name)
            .expect("resolved above")
            .clone();
        let plan = plan.clone();
        let sessions = sessions.clone();
        let engine = config.builder.engine;
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
                let ctx = EndpointDeploymentContext {
                    session,
                    plan: &plan,
                    server,
                    endpoint,
                    service_name: &service_name,
                    service: &service,
                    engine,
                    image: &image,
                    resolved_env: &resolved_env,
                    project_root: &project_root,
                    skip_proxy,
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
    close_all(&tunnel_sessions).await;
    close_all(&sessions).await;

    Ui::section("Deployment Summary:");
    let mut failures = 0usize;
    for outcomes in &results {
        for (identity, outcome) in outcomes {
            match outcome {
                EndpointOutcome::Deployed { candidate_slot } => {
                    Ui::say(&format!("{identity}: deployed (slot {candidate_slot})"), 1);
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
        anyhow::bail!("Deploy failed for {failures} endpoint(s); see the summary above.");
    }

    Ui::success("\nDeployment completed.");
    Ok(())
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

fn hosts_serving_build_configured_services(
    selected: &[&ServiceEndpointPlan],
    services_to_build: &BTreeSet<String>,
) -> BTreeSet<String> {
    selected
        .iter()
        .filter(|endpoint| services_to_build.contains(&endpoint.service))
        .map(|endpoint| endpoint.server.clone())
        .collect()
}
