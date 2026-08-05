use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{IsTerminal, Write};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use jiji_agent::api::{RequestBody, ResponseBody};
use jiji_agent::catalog::{DeploymentState, HealthState};
use jiji_agent::desired::ReplicaAssignment as DesiredAssignment;
use jiji_config::{validate_config, NamedServer};
use jiji_network::{NetworkPlanner, ServiceEndpointPlan};
use jiji_ssh::{SshPool, SshSession};
use jiji_tui::Ui;

use crate::commands::deploy::DEFAULT_MAX_DIR_UPLOAD_BYTES;
use crate::deploy_transaction::{deploy_endpoint, EndpointDeploymentContext, EndpointOutcome};
use crate::lock::{LockRequest, LockScope};
use crate::{container_ops, container_runtime, env_resolution, placement, ssh_adapter};

#[allow(clippy::too_many_arguments)]
pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    hosts: Option<&str>,
    services: Option<&str>,
    replicas: Option<u32>,
    reset: bool,
    dry_run: bool,
    yes: bool,
    host_env: bool,
) -> anyhow::Result<()> {
    Ui::section("Service Scale:");
    let started = std::time::Instant::now();
    if hosts.is_some() {
        anyhow::bail!(
            "`jiji service scale` does not accept -H/--hosts; placement is computed across the service's configured eligible servers"
        );
    }
    let service_filter = services.ok_or_else(|| {
        anyhow::anyhow!("`jiji service scale` requires -S/--services to select exactly one service")
    })?;
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
    let matched = config
        .services
        .keys()
        .filter(|name| jiji_core::matches_pattern(name, service_filter))
        .cloned()
        .collect::<Vec<_>>();
    if matched.len() != 1 {
        anyhow::bail!(
            "-S/--services must match exactly one service; '{}' matched {}",
            service_filter,
            matched.len()
        );
    }
    let service_name = &matched[0];
    let service = &config.services[service_name];
    let ssh = config
        .ssh
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No `ssh:` section configured in {}", path.display()))?;
    let plan = NetworkPlanner::new()
        .plan(&config)
        .context("Could not build the private network plan")?;

    let mut eligible = service.servers.clone();
    eligible.sort();
    eligible.dedup();
    let seed_name = eligible.first().expect("validation requires servers");
    let seed_sessions = connect(&config.servers, ssh, std::slice::from_ref(seed_name)).await?;
    let seed = &seed_sessions[seed_name];
    let current_desired = match crate::agent_client::call(
        seed,
        &config.project,
        None,
        RequestBody::DesiredRead {
            service: service_name.clone(),
        },
    )
    .await?
    {
        ResponseBody::DesiredState { record } => record,
        response => anyhow::bail!("Agent returned unexpected desired-state response: {response:?}"),
    };
    let catalog = crate::agent_client::catalog(seed, &config.project).await?;
    let current_count = current_desired
        .as_ref()
        .map(|record| record.assignments.len() as u32)
        .unwrap_or(service.replicas);
    let requested = if reset {
        service.replicas
    } else if let Some(replicas) = replicas {
        replicas
    } else if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        print!(
            "Desired replicas for '{}' [{}]: ",
            service_name, current_count
        );
        std::io::stdout().flush()?;
        let mut value = String::new();
        std::io::stdin().read_line(&mut value)?;
        if value.trim().is_empty() {
            current_count
        } else {
            value
                .trim()
                .parse()
                .context("Replica count must be a non-negative integer")?
        }
    } else {
        anyhow::bail!(
            "`--replicas N` is required when `jiji service scale` is not attached to a terminal"
        );
    };
    if requested > 2_000 {
        anyhow::bail!("Replica count {requested} exceeds Jiji's supported limit of 2000");
    }
    if requested > 1 {
        if service.stop_first {
            anyhow::bail!("Service '{service_name}' uses stop_first and must remain a singleton");
        }
        if service.network_mode != "bridge" {
            anyhow::bail!("Service '{service_name}' can only scale with project bridge networking");
        }
        if !service.volumes.is_empty()
            || !service.files.is_empty()
            || !service.directories.is_empty()
        {
            anyhow::bail!(
                "Service '{service_name}' has local volumes/files/directories and cannot be scaled implicitly"
            );
        }
        if service.privileged || !service.devices.is_empty() || service.gpus.is_some() {
            anyhow::bail!(
                "Service '{service_name}' uses exclusive host resources and cannot be scaled"
            );
        }
        if !service.ports.is_empty() && requested as usize > eligible.len() {
            anyhow::bail!(
                "Service '{service_name}' publishes fixed host ports and cannot place more than one replica on each of its {} eligible hosts",
                eligible.len()
            );
        }
    }
    let assignments = placement::place(
        &config.project,
        service_name,
        requested,
        &eligible,
        service.placement,
    );
    let declared_current_assignments = current_desired
        .as_ref()
        .map(|record| {
            record
                .assignments
                .iter()
                .map(|assignment| placement::ReplicaAssignment {
                    replica_id: assignment.replica_id.clone(),
                    ordinal: assignment.ordinal,
                    server: assignment.owner_node_id.clone(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| {
            placement::place(
                &config.project,
                service_name,
                service.replicas,
                &eligible,
                service.placement,
            )
        });
    let active_replica_ids = catalog
        .iter()
        .filter(|record| {
            record.service == *service_name
                && record.state == DeploymentState::Active
                && record.health == HealthState::Healthy
        })
        .map(|record| record.replica_id.as_str())
        .collect::<BTreeSet<_>>();
    let desired_ids = assignments
        .iter()
        .map(|assignment| assignment.replica_id.as_str())
        .collect::<BTreeSet<_>>();
    let additions = assignments
        .iter()
        .filter(|assignment| !active_replica_ids.contains(assignment.replica_id.as_str()))
        .collect::<Vec<_>>();
    let mut removals = declared_current_assignments
        .iter()
        .filter(|assignment| !desired_ids.contains(assignment.replica_id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    for record in catalog.iter().filter(|record| {
        record.service == *service_name
            && record.state == DeploymentState::Active
            && !desired_ids.contains(record.replica_id.as_str())
    }) {
        if removals
            .iter()
            .all(|assignment| assignment.replica_id != record.replica_id)
        {
            removals.push(placement::ReplicaAssignment {
                replica_id: record.replica_id.clone(),
                ordinal: u32::MAX,
                server: record.owner_node_id.clone(),
            });
        }
    }

    Ui::say(&format!("Service: {service_name}"), 1);
    Ui::say(&format!("Configured replicas: {}", service.replicas), 1);
    Ui::say(&format!("Current desired replicas: {current_count}"), 1);
    Ui::say(&format!("New desired replicas: {requested}"), 1);
    Ui::say(
        &format!("Placement: {:?}", service.placement).to_lowercase(),
        1,
    );
    for assignment in &additions {
        Ui::say(
            &format!("add {} on {}", assignment.replica_id, assignment.server),
            2,
        );
    }
    for assignment in &removals {
        Ui::say(
            &format!(
                "retire {} from {}",
                assignment.replica_id, assignment.server
            ),
            2,
        );
    }
    if dry_run {
        close_all(&seed_sessions).await;
        Ui::success_elapsed("Scale plan completed; no changes made.", started.elapsed());
        return Ok(());
    }
    if !yes
        && !Ui::confirm(
            &format!("Scale '{service_name}' from {current_count} to {requested} replicas?"),
            false,
        )?
    {
        close_all(&seed_sessions).await;
        anyhow::bail!("Scaling cancelled.");
    }

    let mut affected = additions
        .iter()
        .map(|assignment| assignment.server.clone())
        .chain(removals.iter().map(|assignment| assignment.server.clone()))
        .collect::<BTreeSet<_>>();
    if service.proxy.is_some() {
        // Every eligible service host is an ingress owner. This includes a
        // former replica host whose route must be withdrawn on scale-to-zero.
        affected.extend(eligible.iter().cloned());
    }
    if affected.is_empty() {
        affected.insert(seed_name.clone());
    }
    let affected = affected.into_iter().collect::<Vec<_>>();

    // Connect once, before locking: merge the seed session (already open) in if it's part of the
    // affected set, otherwise close it since nothing further needs it.
    let pool = SshPool::new(ssh.max_concurrent_starts as usize);
    let mut sessions: BTreeMap<String, Arc<SshSession>> = BTreeMap::new();
    if affected.contains(seed_name) {
        sessions.insert(
            seed_name.clone(),
            seed_sessions
                .get(seed_name)
                .expect("connected above")
                .clone(),
        );
    } else {
        close_all(&seed_sessions).await;
    }
    let remaining: Vec<String> = affected
        .iter()
        .filter(|name| !sessions.contains_key(*name))
        .cloned()
        .collect();
    if !remaining.is_empty() {
        match connect(&config.servers, ssh, &remaining).await {
            Ok(more) => sessions.extend(more),
            Err(error) => {
                close_all(&sessions).await;
                return Err(error);
            }
        }
    }

    // A single lock keyed by service name, held on the canonical `eligible[0]` host: two scales
    // of the *same* service always serialize regardless of which hosts either touches, but two
    // different services' scales never contend even when they share hosts.
    let mut lock_requests = vec![LockRequest::new(
        LockScope::ServiceScale {
            service: service_name.clone(),
        },
        seed_name.clone(),
    )];
    if service.proxy.is_some() {
        for host in &affected {
            lock_requests.push(LockRequest::new(LockScope::HostGlobalProxy, host.clone()));
        }
    }

    let scale_result = crate::commands::lock::with_locks(
        &pool,
        &sessions,
        &config.project,
        lock_requests,
        format!("jiji service scale: {service_name} -> {requested}"),
        crate::commands::lock::AutomaticLockOptions {
            timeout: 300,
            force: false,
        },
        || async {
    let seed = sessions
        .values()
        .next()
        .expect("at least one affected session is connected");
    let operation_result: anyhow::Result<()> = async {

    let desired = assignments
        .iter()
        .map(|assignment| DesiredAssignment {
            replica_id: assignment.replica_id.clone(),
            ordinal: assignment.ordinal,
            owner_node_id: assignment.server.clone(),
        })
        .collect();
    let desired_is_current = current_desired.as_ref().is_some_and(|record| {
        record.replica_override == (!reset).then_some(requested)
            && record.assignments == desired
    });
    if !desired_is_current {
        let source_revision = current_desired
            .as_ref()
            .map(|record| record.revision)
            .unwrap_or(0);
        match crate::agent_client::call(
            seed,
            &config.project,
            Some(format!(
                "desired:{service_name}:{}:{source_revision}:{requested}",
                if reset { "reset" } else { "override" }
            )),
            RequestBody::DesiredCommit {
                service: service_name.clone(),
                replica_override: (!reset).then_some(requested),
                assignments: desired,
            },
        )
        .await?
        {
            ResponseBody::DesiredState { record: Some(_) } => {}
            response => {
                anyhow::bail!("Agent returned unexpected desired-state response: {response:?}")
            }
        }
    }

    let project_root = env_resolution::project_root_from_config_path(&path);
    let (loaded_env, _) =
        env_resolution::load_env_file(&project_root, environment, config.secrets_path.as_deref())?;
    let merged = env_resolution::merge_environment(
        &config.environment.clone().unwrap_or_default(),
        &service.environment,
    );
    let resolved_env = env_resolution::resolve_environment(&merged, &loaded_env, host_env)?;
    let image = if let Some(image) = &service.image {
        container_runtime::resolve_image_reference(image, None)?
    } else {
        catalog
            .iter()
            .find(|record| {
                record.service == *service_name
                    && record.state == DeploymentState::Active
                    && record.health == HealthState::Healthy
            })
            .map(|record| record.image.clone())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Build-only service '{service_name}' has no active deployment to copy an image from; deploy it with `jiji deploy --build` first"
                )
            })?
    };

    for assignment in additions {
        let endpoint = endpoint_for_assignment(&plan, service_name, assignment)?;
        let session = &sessions[&assignment.server];
        let server = &plan.servers[&assignment.server];
        let ctx = EndpointDeploymentContext {
            session,
            plan: &plan,
            server,
            endpoint: &endpoint,
            service_name,
            replica_id: &assignment.replica_id,
            service,
            engine: config.builder.engine,
            image: &image,
            resolved_env: &resolved_env,
            project_root: &project_root,
            skip_proxy: false,
            max_dir_upload_bytes: DEFAULT_MAX_DIR_UPLOAD_BYTES,
            progress: None,
        };
        match deploy_endpoint(&ctx).await {
            EndpointOutcome::Deployed { deployment_id, .. } => Ui::result_ok(
                &assignment.replica_id,
                &format!("active on {} ({})", assignment.server, &deployment_id[..12]),
            ),
            EndpointOutcome::Failed { error } => {
                anyhow::bail!(
                    "Scale-up failed for '{}' on '{}': {error}. Retry the same scale command to resume.",
                    assignment.replica_id,
                    assignment.server
                );
            }
            EndpointOutcome::SkippedAfterSiblingFailure => unreachable!(),
        }
    }

            for assignment in removals {
        retire_replica(
            &sessions[&assignment.server],
            &config.project,
            service_name,
            &assignment.replica_id,
            config.builder.engine,
        )
        .await?;
        Ui::result_ok(
            &assignment.replica_id,
            &format!("retired from {}", assignment.server),
                );
            }
            if let Some(proxy) = &service.proxy {
                crate::proxy_routes::reconcile_catalog_routes(
                    &sessions,
                    &config.project,
                    config.builder.engine,
                    &BTreeMap::from([(service_name.clone(), proxy.clone())]),
                )
                .await?;
            }
            Ok(())
    }
    .await;
    for (name, session) in &sessions {
        crate::audit::record(
            session,
            &config.project,
            "service_scale",
            if operation_result.is_ok() {
                crate::audit::AuditStatus::Success
            } else {
                crate::audit::AuditStatus::Failed
            },
            format!("{service_name}: {current_count} -> {requested} replicas on {name}"),
            Some(
                &LockScope::ServiceScale {
                    service: service_name.clone(),
                }
                .to_string(),
            ),
            None,
            Some(started.elapsed()),
        )
        .await;
    }
    operation_result
        },
    )
    .await;
    close_all(&sessions).await;
    scale_result?;
    Ui::success_elapsed("Service scale completed.", started.elapsed());
    Ok(())
}

fn endpoint_for_assignment(
    plan: &jiji_network::NetworkPlan,
    service: &str,
    assignment: &placement::ReplicaAssignment,
) -> anyhow::Result<ServiceEndpointPlan> {
    let mut endpoint = plan
        .endpoints
        .values()
        .find(|endpoint| endpoint.service == service && endpoint.server == assignment.server)
        .cloned()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "No network endpoint exists for service '{service}' on '{}'",
                assignment.server
            )
        })?;
    endpoint.identity = format!("{}:{service}:{}", plan.project, assignment.replica_id);
    Ok(endpoint)
}

async fn retire_replica(
    session: &SshSession,
    project: &str,
    service: &str,
    replica_id: &str,
    engine: jiji_config::ContainerEngine,
) -> anyhow::Result<()> {
    let records = crate::agent_client::catalog(session, project).await?;
    for record in records.into_iter().filter(|record| {
        record.replica_id == replica_id
            && !matches!(
                record.state,
                DeploymentState::Stopped | DeploymentState::Tombstoned
            )
    }) {
        let name =
            container_runtime::dynamic_container_name(project, service, &record.deployment_id);
        container_ops::stop_if_running(session, engine, &name).await?;
        container_ops::remove_if_present(session, engine, &name).await?;
        crate::agent_client::call(
            session,
            project,
            Some(format!("scale:stop:{}", record.deployment_id)),
            RequestBody::CatalogCommit {
                service: service.to_string(),
                replica_id: replica_id.to_string(),
                deployment_id: record.deployment_id.clone(),
                address: record.address.to_string(),
                ports: record.ports,
                image: record.image,
                state: DeploymentState::Tombstoned,
                health: HealthState::Unhealthy,
            },
        )
        .await?;
        crate::agent_client::call(
            session,
            project,
            Some(format!("scale:release:{}", record.deployment_id)),
            RequestBody::ReleaseAddress {
                deployment_id: record.deployment_id,
                timestamp: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            },
        )
        .await?;
    }
    Ok(())
}

async fn connect(
    configured: &HashMap<String, NamedServer>,
    ssh: &jiji_config::Ssh,
    names: &[String],
) -> anyhow::Result<BTreeMap<String, Arc<SshSession>>> {
    let mut sessions = BTreeMap::new();
    for name in names {
        let server = configured
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("Server '{name}' is not configured"))?;
        let options = ssh_adapter::connect_options(name, server, ssh)?;
        let session = SshSession::connect(&options)
            .await
            .with_context(|| format!("Could not connect to '{name}'"))?;
        Ui::say(&format!("{name} ({}): connected", server.host), 1);
        sessions.insert(name.clone(), Arc::new(session));
    }
    Ok(sessions)
}

async fn close_all(sessions: &BTreeMap<String, Arc<SshSession>>) {
    for session in sessions.values() {
        session.close().await;
    }
}
