use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use jiji_agent::api::{RequestBody, ResponseBody};
use jiji_agent::catalog::{CatalogRecord, DeploymentState, HealthState};
use jiji_config::{ContainerEngine, Service};
use jiji_network::{NetworkPlan, ServerPlan, ServiceEndpointPlan};
use jiji_ssh::SshSession;

use crate::env_resolution::{redact_secrets, ResolvedEnvironment};
use crate::proxy_routes::{RouteTarget, TcpRouteTarget};
use crate::{container_ops, container_runtime, health_check, mounts, proxy_routes};

pub type EndpointProgress = Arc<dyn Fn(&str, &str) + Send + Sync>;

pub struct EndpointDeploymentContext<'a> {
    pub session: &'a SshSession,
    pub plan: &'a NetworkPlan,
    pub server: &'a ServerPlan,
    pub endpoint: &'a ServiceEndpointPlan,
    pub service_name: &'a str,
    pub replica_id: &'a str,
    pub service: &'a Service,
    pub engine: ContainerEngine,
    pub image: &'a str,
    pub resolved_env: &'a ResolvedEnvironment,
    pub project_root: &'a Path,
    pub skip_proxy: bool,
    pub max_dir_upload_bytes: u64,
    pub progress: Option<EndpointProgress>,
}

#[derive(Debug)]
pub enum EndpointOutcome {
    Deployed { deployment_id: String },
    Failed { error: String },
    SkippedAfterSiblingFailure,
}

pub async fn deploy_endpoint(ctx: &EndpointDeploymentContext<'_>) -> EndpointOutcome {
    let result = if ctx.service.network_mode == "host" {
        deploy_host_mode(ctx).await
    } else {
        match ctx.service.network_mode_dependency() {
            Some(upstream_service_name) => deploy_shared_endpoint(ctx, upstream_service_name).await,
            None => deploy_dynamic_endpoint(ctx).await,
        }
    };
    match result {
        Ok((deployment_id, _address)) => EndpointOutcome::Deployed { deployment_id },
        Err(error) => EndpointOutcome::Failed {
            error: error.to_string(),
        },
    }
}

/// Deploys a `network_mode: service:<upstream>` dependent: a container that shares another
/// service's network namespace instead of getting its own dynamically-leased address. Validation
/// (`jiji_config::validation`) already guarantees the upstream isn't itself a dependent (no
/// chains) and that this service has no `proxy:` of its own (traffic reaches it through the
/// upstream's route, at the upstream's own address -- never a route of its own).
async fn deploy_shared_endpoint(
    ctx: &EndpointDeploymentContext<'_>,
    upstream_service_name: &str,
) -> anyhow::Result<(String, std::net::Ipv4Addr)> {
    use sha2::{Digest, Sha256};

    let project = &ctx.plan.project;
    let replica_id = ctx.replica_id.to_string();
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let deployment_id = Sha256::digest(
        format!("{project}\0{replica_id}\0{nonce}\0{}", std::process::id()).as_bytes(),
    )
    .iter()
    .map(|byte| format!("{byte:02x}"))
    .collect::<String>();
    let candidate_name =
        container_runtime::dynamic_container_name(project, ctx.service_name, &deployment_id);

    let catalog = crate::agent_client::catalog(ctx.session, project).await?;
    let previous = catalog
        .iter()
        .find(|record| {
            record.replica_id == replica_id
                && record.state == DeploymentState::Active
                && record.health == HealthState::Healthy
        })
        .cloned();

    // Resolved by service+server, not by recomputing the upstream's replica_id: a dependent is
    // always scale 1 and has no local_index of its own to derive the upstream's from. This
    // assumes the upstream itself is also effectively scale 1 on this server -- if the upstream
    // runs `scale > 1`, `.find()` below picks whichever of its Active/Healthy replicas the
    // catalog iterator happens to return first, not necessarily local_index 0. Known limitation,
    // not introduced or fixed by this change: `network_mode: service:<name>` sharing was never
    // validated against the upstream's own scale.
    let upstream = catalog
        .iter()
        .find(|record| {
            record.service == upstream_service_name
                && record.owner_node_id == ctx.server.name
                && record.state == DeploymentState::Active
                && record.health == HealthState::Healthy
        })
        .cloned()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Service '{}' shares '{upstream_service_name}''s network namespace, but \
                 '{upstream_service_name}' has no active, healthy deployment on '{}'. Deploy \
                 '{upstream_service_name}' first.",
                ctx.service_name,
                ctx.server.name
            )
        })?;
    let target_container_name = container_runtime::dynamic_container_name(
        project,
        upstream_service_name,
        &upstream.deployment_id,
    );

    container_ops::ensure_image(ctx.session, ctx.engine, ctx.image).await?;
    let mount_args = mounts::prepare_mounts(
        ctx.session,
        ctx.service,
        ctx.service_name,
        project,
        ctx.project_root,
        ctx.max_dir_upload_bytes,
    )
    .await?;
    let env_file_path = crate::env_resolution::stage_env_file(
        ctx.session,
        project,
        ctx.service_name,
        &ctx.server.name,
        ctx.resolved_env,
    )
    .await?;
    let run = container_runtime::build_shared_run(
        ctx.engine,
        project,
        ctx.service_name,
        &ctx.server.name,
        &replica_id,
        &deployment_id,
        ctx.image,
        &target_container_name,
        upstream.address,
        ctx.server,
        ctx.service,
        &mount_args,
        &env_file_path,
    );

    commit_catalog(
        ctx,
        &replica_id,
        &deployment_id,
        upstream.address,
        Vec::new(),
        DeploymentState::Candidate,
        HealthState::Unknown,
    )
    .await?;

    report_progress(ctx, "starting container");
    if let Err(error) = container_ops::create_and_start(ctx.session, &run).await {
        release_candidate(
            ctx,
            &replica_id,
            &deployment_id,
            upstream.address,
            &candidate_name,
        )
        .await;
        return Err(error);
    }

    // No healthcheck config exists for a dependent (validation forbids its own `proxy:`, the only
    // place a healthcheck lives today), so this falls through to `plan_for_candidate`'s existing
    // "no healthcheck configured" fallback: an engine-native container-readiness check, the same
    // one any bridge-networked service without an explicit `healthcheck:` already gets.
    let health_plan =
        health_check::plan_for_candidate(ctx.engine, &candidate_name, upstream.address, 0, None);
    report_progress(
        ctx,
        &health_check_progress_detail(health_plan.deploy_timeout),
    );
    if let Err(error) = health_check::wait_until_healthy(
        ctx.session,
        ctx.engine,
        &candidate_name,
        &health_plan,
        |line| health_check_line_progress(ctx, line),
    )
    .await
    {
        release_candidate(
            ctx,
            &replica_id,
            &deployment_id,
            upstream.address,
            &candidate_name,
        )
        .await;
        let message = redact_secrets(&error.to_string(), ctx.resolved_env);
        return Err(anyhow::anyhow!(message));
    }

    commit_catalog(
        ctx,
        &replica_id,
        &deployment_id,
        upstream.address,
        Vec::new(),
        DeploymentState::Active,
        HealthState::Healthy,
    )
    .await?;

    if let Some(previous) = previous {
        commit_catalog(
            ctx,
            &previous.replica_id,
            &previous.deployment_id,
            previous.address,
            previous.ports.clone(),
            DeploymentState::Draining,
            HealthState::Unknown,
        )
        .await?;
        let old_name = container_runtime::dynamic_container_name(
            project,
            ctx.service_name,
            &previous.deployment_id,
        );
        let orphaned_image =
            capture_orphaned_static_image(ctx, ctx.service_name, Some(ctx.service), &old_name)
                .await;
        let _ = container_ops::stop(ctx.session, ctx.engine, &old_name).await;
        container_ops::remove_if_present(ctx.session, ctx.engine, &old_name).await?;
        finish_orphaned_static_image_cleanup(ctx, orphaned_image).await;
        // No lease was ever allocated for a dependent, so there is nothing to release here --
        // only its catalog record needs to settle to Tombstoned.
        commit_catalog(
            ctx,
            &previous.replica_id,
            &previous.deployment_id,
            previous.address,
            previous.ports,
            DeploymentState::Tombstoned,
            HealthState::Unknown,
        )
        .await?;
    }
    sweep_stuck_draining_records(ctx, upstream_service_name, None).await;
    Ok((deployment_id, upstream.address))
}

async fn deploy_dynamic_endpoint(
    ctx: &EndpointDeploymentContext<'_>,
) -> anyhow::Result<(String, std::net::Ipv4Addr)> {
    use sha2::{Digest, Sha256};

    let project = &ctx.plan.project;
    let replica_id = ctx.replica_id.to_string();
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let deployment_id = Sha256::digest(
        format!("{project}\0{replica_id}\0{nonce}\0{}", std::process::id()).as_bytes(),
    )
    .iter()
    .map(|byte| format!("{byte:02x}"))
    .collect::<String>();
    let candidate_name =
        container_runtime::dynamic_container_name(project, ctx.service_name, &deployment_id);
    let catalog = crate::agent_client::catalog(ctx.session, project).await?;
    let previous = catalog
        .iter()
        .find(|record| {
            record.replica_id == replica_id
                && record.state == DeploymentState::Active
                && record.health == HealthState::Healthy
        })
        .cloned();

    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let lease = crate::agent_client::call(
        ctx.session,
        project,
        Some(format!("lease:{deployment_id}")),
        RequestBody::AllocateAddress {
            deployment_id: deployment_id.clone(),
            replica_id: replica_id.clone(),
            subnet: ctx.server.container_subnet.to_string(),
            reserved: vec![
                ctx.server.bridge_gateway.to_string(),
                ctx.server.dns_address.to_string(),
                ctx.server.proxy_address.to_string(),
            ],
            timestamp,
        },
    )
    .await?;
    let address = match lease {
        ResponseBody::AddressLease { address, .. } => address.parse()?,
        response => anyhow::bail!("Agent returned unexpected lease response: {response:?}"),
    };
    report_progress(ctx, &format!("leased {address}"));

    if ctx.service.stop_first {
        if let Some(previous) = &previous {
            let previous_name = container_runtime::dynamic_container_name(
                project,
                ctx.service_name,
                &previous.deployment_id,
            );
            container_ops::stop(ctx.session, ctx.engine, &previous_name).await?;
        }
    }

    container_ops::ensure_image(ctx.session, ctx.engine, ctx.image).await?;
    let mount_args = mounts::prepare_mounts(
        ctx.session,
        ctx.service,
        ctx.service_name,
        project,
        ctx.project_root,
        ctx.max_dir_upload_bytes,
    )
    .await?;
    let env_file_path = crate::env_resolution::stage_env_file(
        ctx.session,
        project,
        ctx.service_name,
        &ctx.server.name,
        ctx.resolved_env,
    )
    .await?;
    let run = container_runtime::build_dynamic_run(
        ctx.engine,
        project,
        ctx.service_name,
        &ctx.server.name,
        &replica_id,
        &deployment_id,
        ctx.image,
        address,
        ctx.server,
        ctx.service,
        &mount_args,
        &env_file_path,
    );

    let dns_server = std::net::SocketAddr::new(ctx.server.dns_address.into(), 53);
    let mut targets = proxy_routes::targets_for_service(
        project,
        ctx.service_name,
        ctx.service.proxy.as_ref(),
        dns_server,
    )?;
    proxy_routes::resolve_tls_secrets(&mut targets, ctx.resolved_env)?;
    let tcp_targets = proxy_routes::tcp_targets_for_service(
        project,
        ctx.service_name,
        ctx.service.proxy.as_ref(),
        dns_server,
    )?;
    let committed_ports: Vec<u16> = targets
        .iter()
        .map(|target| target.port)
        .chain(tcp_targets.iter().map(|target| target.port))
        .collect();
    commit_catalog(
        ctx,
        &replica_id,
        &deployment_id,
        address,
        committed_ports.clone(),
        DeploymentState::Candidate,
        HealthState::Unknown,
    )
    .await?;

    report_progress(ctx, "starting container");
    if let Err(error) = container_ops::create_and_start(ctx.session, &run).await {
        release_candidate(ctx, &replica_id, &deployment_id, address, &candidate_name).await;
        restore_stop_first(ctx, previous.as_ref()).await;
        return Err(error);
    }

    let (health_port, health_config) = targets
        .first()
        .map(|target| (target.port, target.healthcheck.as_ref()))
        .or_else(|| {
            tcp_targets
                .first()
                .map(|target| (target.port, target.healthcheck.as_ref()))
        })
        .unwrap_or((0, None));
    let health_plan = health_check::plan_for_candidate(
        ctx.engine,
        &candidate_name,
        address,
        health_port.into(),
        health_config,
    );
    report_progress(
        ctx,
        &health_check_progress_detail(health_plan.deploy_timeout),
    );
    if let Err(error) = health_check::wait_until_healthy(
        ctx.session,
        ctx.engine,
        &candidate_name,
        &health_plan,
        |line| health_check_line_progress(ctx, line),
    )
    .await
    {
        release_candidate(ctx, &replica_id, &deployment_id, address, &candidate_name).await;
        restore_stop_first(ctx, previous.as_ref()).await;
        let message = redact_secrets(&error.to_string(), ctx.resolved_env);
        return Err(anyhow::anyhow!(message));
    }

    // Commit Active/Healthy before touching jiji-proxy at all: jiji-agent's DNS resolver answers
    // directly from this catalog, so the write itself is what makes the candidate's address
    // resolvable -- there is no address to push to jiji-proxy the way kamal-proxy needed one.
    commit_catalog(
        ctx,
        &replica_id,
        &deployment_id,
        address,
        committed_ports.clone(),
        DeploymentState::Active,
        HealthState::Healthy,
    )
    .await?;

    if !ctx.skip_proxy {
        // "Activation" here means confirming jiji-proxy has actually discovered and health-checked
        // this candidate's address, not pushing an address to it (see `RouteTarget`'s doc comment
        // and `proxy_routes::verify_route_address`): each route's static definition is re-applied
        // (a cheap, idempotent upsert that always runs a synchronous DNS re-resolution -- see
        // `RouteManager::apply`), forcing jiji-proxy to see the catalog write above right away
        // instead of waiting out its own `refresh_interval_secs`; verification then polls until
        // the specific new address shows up healthy or the same timeout this candidate's own
        // pre-activation health check used elapses.
        report_progress(ctx, &proxy_activation_progress_detail(&targets));
        let activation = async {
            activate_proxy_routes(ctx, &targets, address, health_plan.deploy_timeout).await?;
            activate_tcp_proxy_routes(ctx, &tcp_targets, address, health_plan.deploy_timeout).await
        }
        .await;
        if let Err(error) = activation {
            // The candidate is already Active/Healthy in the catalog and jiji-proxy's own DNS
            // re-resolution is mesh-wide, so leaving it there risks other hosts' proxies converging
            // on an address this host's own jiji-proxy couldn't confirm -- roll it back rather than
            // leave an unverified record live. `previous` was never touched (still Active, still
            // serving), so there is no route to "restore": unlike kamal-proxy's single-target
            // routes, this host's route definition never named an address in the first place.
            release_candidate(ctx, &replica_id, &deployment_id, address, &candidate_name).await;
            restore_stop_first(ctx, previous.as_ref()).await;
            return Err(error);
        }
    }

    if let Some(previous) = previous {
        commit_catalog(
            ctx,
            &previous.replica_id,
            &previous.deployment_id,
            previous.address,
            previous.ports.clone(),
            DeploymentState::Draining,
            HealthState::Unknown,
        )
        .await?;
        let old_name = container_runtime::dynamic_container_name(
            project,
            ctx.service_name,
            &previous.deployment_id,
        );
        let orphaned_image =
            capture_orphaned_static_image(ctx, ctx.service_name, Some(ctx.service), &old_name)
                .await;
        let _ = container_ops::stop(ctx.session, ctx.engine, &old_name).await;
        if let Err(error) =
            container_ops::remove_if_present(ctx.session, ctx.engine, &old_name).await
        {
            if !is_dependent_container_error(&error) {
                return Err(error);
            }
            // A `network_mode: service:<this>` dependent hasn't redeployed onto our new
            // candidate yet, so it's still attached to this container's network namespace --
            // confirmed live, Podman refuses removal outright in this case ("has dependent
            // containers which must be removed before it"). Our own candidate is already
            // active and healthy, so this must not fail the whole deploy: leave the previous
            // container running (still `Draining` in the catalog, its address lease intact,
            // exactly as today) rather than releasing an address a still-running container is
            // using. The dependent's own redeploy detaches it, letting a later redeploy of this
            // replica finish the cleanup this one couldn't.
            tracing::warn!(
                container = %old_name,
                service = ctx.service_name,
                "previous container still has a network_mode:service dependent attached; \
                 deferring its removal until that dependent redeploys"
            );
            sweep_stuck_draining_records(ctx, ctx.service_name, Some(ctx.service)).await;
            return Ok((deployment_id, address));
        }
        finish_orphaned_static_image_cleanup(ctx, orphaned_image).await;
        let _ = crate::agent_client::call(
            ctx.session,
            project,
            Some(format!("release:{}", previous.deployment_id)),
            RequestBody::ReleaseAddress {
                deployment_id: previous.deployment_id.clone(),
                timestamp,
            },
        )
        .await?;
        commit_catalog(
            ctx,
            &previous.replica_id,
            &previous.deployment_id,
            previous.address,
            previous.ports,
            DeploymentState::Tombstoned,
            HealthState::Unknown,
        )
        .await?;
    }
    sweep_stuck_draining_records(ctx, ctx.service_name, Some(ctx.service)).await;
    Ok((deployment_id, address))
}

/// Deploys a `network_mode: host` service: a container that shares the host's own network
/// namespace instead of leasing a bridge address. Closer in shape to `deploy_dynamic_endpoint`
/// than to `deploy_shared_endpoint`: it still creates a genuinely new container with the full
/// candidate/active/draining/tombstone catalog lifecycle and a container-readiness health check,
/// it just never calls `AllocateAddress` and uses `ctx.server.management_address` everywhere the
/// dynamic path uses the leased address. `proxy:` is rejected by validation for any non-bridge
/// mode, so there is never a route to activate here (mirrors `deploy_shared_endpoint`'s "no
/// healthcheck config exists" fallback for the same reason).
async fn deploy_host_mode(
    ctx: &EndpointDeploymentContext<'_>,
) -> anyhow::Result<(String, std::net::Ipv4Addr)> {
    use sha2::{Digest, Sha256};

    let project = &ctx.plan.project;
    let replica_id = ctx.replica_id.to_string();
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let deployment_id = Sha256::digest(
        format!("{project}\0{replica_id}\0{nonce}\0{}", std::process::id()).as_bytes(),
    )
    .iter()
    .map(|byte| format!("{byte:02x}"))
    .collect::<String>();
    let candidate_name =
        container_runtime::dynamic_container_name(project, ctx.service_name, &deployment_id);
    let address = ctx.server.management_address;
    let committed_ports: Vec<u16> = ctx
        .service
        .ports
        .first()
        .and_then(|port| port.parse::<u16>().ok())
        .into_iter()
        .collect();

    let catalog = crate::agent_client::catalog(ctx.session, project).await?;
    let previous = catalog
        .iter()
        .find(|record| {
            record.replica_id == replica_id
                && record.state == DeploymentState::Active
                && record.health == HealthState::Healthy
        })
        .cloned();

    if ctx.service.stop_first {
        if let Some(previous) = &previous {
            let previous_name = container_runtime::dynamic_container_name(
                project,
                ctx.service_name,
                &previous.deployment_id,
            );
            container_ops::stop(ctx.session, ctx.engine, &previous_name).await?;
        }
    }

    container_ops::ensure_image(ctx.session, ctx.engine, ctx.image).await?;
    let mount_args = mounts::prepare_mounts(
        ctx.session,
        ctx.service,
        ctx.service_name,
        project,
        ctx.project_root,
        ctx.max_dir_upload_bytes,
    )
    .await?;
    let env_file_path = crate::env_resolution::stage_env_file(
        ctx.session,
        project,
        ctx.service_name,
        &ctx.server.name,
        ctx.resolved_env,
    )
    .await?;
    let run = container_runtime::build_host_run(
        ctx.engine,
        project,
        ctx.service_name,
        &ctx.server.name,
        &replica_id,
        &deployment_id,
        ctx.image,
        address,
        ctx.server,
        ctx.service,
        &mount_args,
        &env_file_path,
    );

    commit_catalog(
        ctx,
        &replica_id,
        &deployment_id,
        address,
        committed_ports.clone(),
        DeploymentState::Candidate,
        HealthState::Unknown,
    )
    .await?;

    report_progress(ctx, "starting container");
    if let Err(error) = container_ops::create_and_start(ctx.session, &run).await {
        release_candidate(ctx, &replica_id, &deployment_id, address, &candidate_name).await;
        restore_stop_first(ctx, previous.as_ref()).await;
        return Err(error);
    }

    let health_plan =
        health_check::plan_for_candidate(ctx.engine, &candidate_name, address, 0, None);
    report_progress(
        ctx,
        &health_check_progress_detail(health_plan.deploy_timeout),
    );
    if let Err(error) = health_check::wait_until_healthy(
        ctx.session,
        ctx.engine,
        &candidate_name,
        &health_plan,
        |line| health_check_line_progress(ctx, line),
    )
    .await
    {
        release_candidate(ctx, &replica_id, &deployment_id, address, &candidate_name).await;
        restore_stop_first(ctx, previous.as_ref()).await;
        let message = redact_secrets(&error.to_string(), ctx.resolved_env);
        return Err(anyhow::anyhow!(message));
    }

    commit_catalog(
        ctx,
        &replica_id,
        &deployment_id,
        address,
        committed_ports.clone(),
        DeploymentState::Active,
        HealthState::Healthy,
    )
    .await?;

    if let Some(previous) = previous {
        commit_catalog(
            ctx,
            &previous.replica_id,
            &previous.deployment_id,
            previous.address,
            previous.ports.clone(),
            DeploymentState::Draining,
            HealthState::Unknown,
        )
        .await?;
        let old_name = container_runtime::dynamic_container_name(
            project,
            ctx.service_name,
            &previous.deployment_id,
        );
        let orphaned_image =
            capture_orphaned_static_image(ctx, ctx.service_name, Some(ctx.service), &old_name)
                .await;
        let _ = container_ops::stop(ctx.session, ctx.engine, &old_name).await;
        if let Err(error) =
            container_ops::remove_if_present(ctx.session, ctx.engine, &old_name).await
        {
            if !is_dependent_container_error(&error) {
                return Err(error);
            }
            tracing::warn!(
                container = %old_name,
                service = ctx.service_name,
                "previous container still has a network_mode:service dependent attached; \
                 deferring its removal until that dependent redeploys"
            );
            sweep_stuck_draining_records(ctx, ctx.service_name, Some(ctx.service)).await;
            return Ok((deployment_id, address));
        }
        finish_orphaned_static_image_cleanup(ctx, orphaned_image).await;
        // No lease was ever allocated for a host-mode container, but the release call stays
        // unconditional (matching `deploy_shared_endpoint`'s precedent): the agent's release
        // handler already no-ops correctly when nothing was ever leased for this `deployment_id`.
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let _ = crate::agent_client::call(
            ctx.session,
            project,
            Some(format!("release:{}", previous.deployment_id)),
            RequestBody::ReleaseAddress {
                deployment_id: previous.deployment_id.clone(),
                timestamp,
            },
        )
        .await?;
        commit_catalog(
            ctx,
            &previous.replica_id,
            &previous.deployment_id,
            previous.address,
            previous.ports,
            DeploymentState::Tombstoned,
            HealthState::Unknown,
        )
        .await?;
    }
    sweep_stuck_draining_records(ctx, ctx.service_name, Some(ctx.service)).await;
    Ok((deployment_id, address))
}

/// Sweeps `service_name`'s catalog for any stuck `Draining` records left behind by an earlier
/// redeploy whose old container couldn't be removed because a `network_mode: service:<this>`
/// dependent was still attached at the time (see `is_dependent_container_error`), retrying their
/// cleanup now. Best-effort and non-fatal: never fails the deploy that called it. Called both by a
/// service's own redeploy (for records left by an earlier redeploy of itself) and by any of its
/// dependents' redeploys (for records left behind because that exact dependent was the one
/// blocking removal) -- whichever happens first finishes the interrupted cleanup, since a
/// `Draining` record's own service only ever looks for its *current* `Active` record on its own
/// next redeploy, never revisiting an older `Draining` leftover by itself.
///
/// `service` is `service_name`'s own config, when the caller has it, used the same way the two
/// normal cutover paths do to capture-then-finish an orphaned static image around the removal
/// this sweep retries here: the digest a blocked removal's own `capture_orphaned_static_image`
/// captured is local to that one failed attempt and is lost the moment it returns early without
/// ever reaching `finish_orphaned_static_image_cleanup` -- this sweep is that record's only other
/// chance, since nothing else re-visits an old `Draining` leftover by itself. `None` when called
/// for an upstream from a dependent's own redeploy (`EndpointDeploymentContext` has no `Config` to
/// look the upstream's own `build:` setting up there): `capture_orphaned_static_image` still
/// protects a build-managed upstream image in that case, via its own live retention-spec check.
async fn sweep_stuck_draining_records(
    ctx: &EndpointDeploymentContext<'_>,
    service_name: &str,
    service: Option<&Service>,
) {
    let project = &ctx.plan.project;
    let Ok(catalog) = crate::agent_client::catalog(ctx.session, project).await else {
        return;
    };
    let stuck: Vec<_> = catalog
        .into_iter()
        .filter(|record| {
            record.service == service_name
                && record.owner_node_id == ctx.server.name
                && record.state == DeploymentState::Draining
        })
        .collect();
    for record in stuck {
        let old_name =
            container_runtime::dynamic_container_name(project, service_name, &record.deployment_id);
        let orphaned_image =
            capture_orphaned_static_image(ctx, service_name, service, &old_name).await;
        if let Err(error) =
            container_ops::remove_if_present(ctx.session, ctx.engine, &old_name).await
        {
            if !is_dependent_container_error(&error) {
                tracing::warn!(
                    container = %old_name,
                    service = service_name,
                    %error,
                    "could not finish deferred cleanup of a previous container"
                );
            }
            continue;
        }
        finish_orphaned_static_image_cleanup(ctx, orphaned_image).await;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or_default();
        let _ = crate::agent_client::call(
            ctx.session,
            project,
            Some(format!("release:{}", record.deployment_id)),
            RequestBody::ReleaseAddress {
                deployment_id: record.deployment_id.clone(),
                timestamp,
            },
        )
        .await;
        let _ = crate::agent_client::call(
            ctx.session,
            project,
            Some(format!("catalog:{}:Tombstoned", record.deployment_id)),
            RequestBody::CatalogCommit {
                service: record.service.clone(),
                replica_id: record.replica_id.clone(),
                deployment_id: record.deployment_id.clone(),
                address: record.address.to_string(),
                ports: record.ports.clone(),
                image: record.image.clone(),
                state: DeploymentState::Tombstoned,
                health: HealthState::Unknown,
            },
        )
        .await;
    }
}

/// Whether `error` indicates a container couldn't be removed only because another container (a
/// `network_mode: service:<this>` dependent still attached to its network namespace) depends on
/// it -- the container engine's own dependency-tracking safety check, not a genuine removal
/// failure (permissions, engine issues, etc.) that should still propagate as before.
fn is_dependent_container_error(error: &anyhow::Error) -> bool {
    error
        .to_string()
        .to_lowercase()
        .contains("dependent container")
}

#[allow(clippy::too_many_arguments)]
async fn commit_catalog(
    ctx: &EndpointDeploymentContext<'_>,
    replica_id: &str,
    deployment_id: &str,
    address: std::net::Ipv4Addr,
    ports: Vec<u16>,
    state: DeploymentState,
    health: HealthState,
) -> anyhow::Result<CatalogRecord> {
    let response = crate::agent_client::call(
        ctx.session,
        &ctx.plan.project,
        Some(format!("catalog:{deployment_id}:{state:?}")),
        RequestBody::CatalogCommit {
            service: ctx.service_name.to_string(),
            replica_id: replica_id.to_string(),
            deployment_id: deployment_id.to_string(),
            address: address.to_string(),
            ports,
            image: ctx.image.to_string(),
            state,
            health,
        },
    )
    .await?;
    match response {
        ResponseBody::CatalogCommitted { record } => Ok(record),
        response => anyhow::bail!("Agent returned unexpected catalog response: {response:?}"),
    }
}

async fn release_candidate(
    ctx: &EndpointDeploymentContext<'_>,
    replica_id: &str,
    deployment_id: &str,
    address: std::net::Ipv4Addr,
    candidate_name: &str,
) {
    let _ = container_ops::remove_if_present(ctx.session, ctx.engine, candidate_name).await;
    let _ = commit_catalog(
        ctx,
        replica_id,
        deployment_id,
        address,
        Vec::new(),
        DeploymentState::Tombstoned,
        HealthState::Unhealthy,
    )
    .await;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let _ = crate::agent_client::call(
        ctx.session,
        &ctx.plan.project,
        Some(format!("release:{deployment_id}")),
        RequestBody::ReleaseAddress {
            deployment_id: deployment_id.to_string(),
            timestamp,
        },
    )
    .await;
}

async fn restore_stop_first(ctx: &EndpointDeploymentContext<'_>, previous: Option<&CatalogRecord>) {
    if !ctx.service.stop_first {
        return;
    }
    if let Some(previous) = previous {
        let name = container_runtime::dynamic_container_name(
            &ctx.plan.project,
            ctx.service_name,
            &previous.deployment_id,
        );
        let _ = container_ops::start(ctx.session, ctx.engine, &name).await;
    }
}

/// For a static `image:` service (no `build:` configured), an old container's image has no
/// rollback value once its container is gone -- there is no tag an operator could ever ask to
/// roll back to (unlike a build-configured service's versioned tags, which `retain:`-based
/// pruning keeps the last N of, see `image_retention_reconcile.rs`). Left alone, a moving tag
/// like `:latest` orphans a new, permanently untagged local image on every redeploy: nothing else
/// in this codebase ever removes it (`image_teardown.rs` only removes an image by its current tag
/// reference, which by then already points at the *new* content). Call before removing `old_name`
/// -- `.Config.Image` (what `inspect_image` reads) is the reference string, identical across every
/// pull of a moving tag; `.Image` (`inspect_image_id`) is the actual resolved digest, the only way
/// to identify precisely which local image entry this removal is about to orphan.
///
/// `service` is `service_name`'s *current* config, when the caller has it: a fast path that
/// avoids the RPC below whenever the currently-deploying service still has `build:` configured.
/// It is never enough on its own, because it describes the config now, not what actually produced
/// the previous container's image -- a service that just dropped `build:` still has an old,
/// `retain:`-tracked image sitting there, and a dependent's redeploy cleaning up an *upstream*'s
/// leftover record has no `Service` for it at all (`None`). Either way,
/// `service_has_retention_spec` is the actual source of truth: a spec is only ever installed for
/// a build-configured service, and `image_retention_reconcile.rs`'s own sweep that removes a spec
/// after `build:` is dropped runs strictly after this deploy's cutover, so a spec still installed
/// here means the image about to be superseded is one `retain:` is tracking, regardless of what
/// `service` says.
async fn capture_orphaned_static_image(
    ctx: &EndpointDeploymentContext<'_>,
    service_name: &str,
    service: Option<&Service>,
    old_name: &str,
) -> Option<String> {
    if service.is_some_and(|service| service.build.is_some()) {
        return None;
    }
    if service_has_retention_spec(ctx, service_name).await {
        return None;
    }
    container_ops::inspect_image_id(ctx.session, ctx.engine, old_name)
        .await
        .ok()
        .flatten()
}

/// Whether `service_name` currently has an image-retention spec installed on this host, queried
/// live rather than inferred from a `Service` config -- see `capture_orphaned_static_image`'s doc
/// comment for why neither the current config nor "no config at all" is reliable here. Fails
/// closed (`false`) on any RPC error (including a too-old agent that doesn't understand this
/// request), matching `image_retention_reconcile.rs::sweep_stale_retention_specs`'s own precedent
/// for this exact request.
async fn service_has_retention_spec(
    ctx: &EndpointDeploymentContext<'_>,
    service_name: &str,
) -> bool {
    match crate::agent_client::call(
        ctx.session,
        &ctx.plan.project,
        None,
        RequestBody::ImageRetentionList,
    )
    .await
    {
        Ok(ResponseBody::ImageRetentionSpecs { specs }) => {
            specs.iter().any(|spec| spec.service == service_name)
        }
        _ => false,
    }
}

/// Removes the image ID `capture_orphaned_static_image` captured, but only once the old
/// container is actually gone and only if nothing else on the host still references it (a
/// same-digest redeploy, or an unrelated container sharing the base layers, must not be torn
/// down). Best-effort: a failure here must never fail the deploy that already succeeded.
async fn finish_orphaned_static_image_cleanup(
    ctx: &EndpointDeploymentContext<'_>,
    orphaned_image_id: Option<String>,
) {
    let Some(image_id) = orphaned_image_id else {
        return;
    };
    let referenced =
        match container_ops::image_referenced_elsewhere(ctx.session, ctx.engine, &image_id).await {
            Ok(referenced) => referenced,
            Err(_) => return,
        };
    if referenced.is_empty() {
        let _ = container_ops::remove_image_if_present(ctx.session, ctx.engine, &image_id).await;
    }
}

fn report_progress(ctx: &EndpointDeploymentContext<'_>, detail: &str) {
    if let Some(progress) = &ctx.progress {
        progress(&ctx.endpoint.identity, detail);
    }
}

fn health_check_progress_detail(timeout: std::time::Duration) -> String {
    format!("health check ({}s)", timeout.as_secs())
}

fn health_check_line_progress(ctx: &EndpointDeploymentContext<'_>, line: &str) {
    report_progress(
        ctx,
        &format!("health check: {}", redact_secrets(line, ctx.resolved_env)),
    );
}

fn proxy_activation_progress_detail(targets: &[RouteTarget]) -> String {
    let timeout = targets
        .iter()
        .filter_map(|target| target.healthcheck.as_ref())
        .filter_map(|check| check.deploy_timeout.as_deref())
        .filter_map(health_check::parse_duration)
        .max();
    match timeout {
        Some(timeout) => format!("proxy route ({}s)", timeout.as_secs()),
        None => "proxy route".to_string(),
    }
}

async fn activate_proxy_routes(
    ctx: &EndpointDeploymentContext<'_>,
    candidate_targets: &[RouteTarget],
    address: std::net::Ipv4Addr,
    timeout: std::time::Duration,
) -> anyhow::Result<()> {
    for target in candidate_targets {
        proxy_routes::deploy_route(ctx.session, ctx.engine, target).await?;
        let expected = std::net::SocketAddr::new(address.into(), target.port);
        proxy_routes::verify_route_address(
            ctx.session,
            ctx.engine,
            &target.host,
            target.path_prefix.as_deref(),
            expected,
            timeout,
        )
        .await?;
    }
    Ok(())
}

/// Mirrors `activate_proxy_routes` for raw TCP targets.
async fn activate_tcp_proxy_routes(
    ctx: &EndpointDeploymentContext<'_>,
    candidate_targets: &[TcpRouteTarget],
    address: std::net::Ipv4Addr,
    timeout: std::time::Duration,
) -> anyhow::Result<()> {
    for target in candidate_targets {
        proxy_routes::deploy_tcp_route(ctx.session, ctx.engine, target).await?;
        let expected = std::net::SocketAddr::new(address.into(), target.port);
        proxy_routes::verify_tcp_route_address(
            ctx.session,
            ctx.engine,
            target.listen_port,
            expected,
            timeout,
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        health_check_progress_detail, is_dependent_container_error,
        proxy_activation_progress_detail, RouteTarget,
    };
    use jiji_config::HealthcheckConfig;
    use std::time::Duration;

    #[test]
    fn recognizes_podmans_live_confirmed_dependent_container_error() {
        let error = anyhow::anyhow!(
            "Command `podman rm -f demo-gluetun-abc` failed on host (exit Some(125)): Error: container fe0e7868216d... has dependent containers which must be removed before it: 433d70a7d655...: container already exists"
        );
        assert!(is_dependent_container_error(&error));
    }

    #[test]
    fn does_not_misclassify_an_unrelated_removal_failure() {
        let error = anyhow::anyhow!(
            "Command `podman rm -f demo-web-abc` failed on host (exit Some(1)): Error: permission denied"
        );
        assert!(!is_dependent_container_error(&error));
    }

    #[test]
    fn health_check_progress_includes_the_configured_timeout() {
        assert_eq!(
            health_check_progress_detail(Duration::from_secs(90)),
            "health check (90s)"
        );
    }

    #[test]
    fn proxy_progress_distinguishes_its_second_health_gate() {
        let targets = vec![RouteTarget {
            host: "example.com".to_string(),
            path_prefix: None,
            dns_server: "127.0.0.1:53".parse().unwrap(),
            name: "demo-web.jiji".to_string(),
            port: 3000,
            ssl: None,
            healthcheck: Some(
                serde_yaml::from_str::<HealthcheckConfig>("deploy_timeout: 60s\n").unwrap(),
            ),
        }];

        assert_eq!(
            proxy_activation_progress_detail(&targets),
            "proxy route (60s)"
        );
    }
}
