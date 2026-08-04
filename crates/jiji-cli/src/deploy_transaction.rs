use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use jiji_agent::api::{RequestBody, ResponseBody};
use jiji_agent::catalog::{CatalogRecord, DeploymentState, HealthState};
use jiji_config::{ContainerEngine, Service};
use jiji_network::{NetworkPlan, ServerPlan, ServiceEndpointPlan};
use jiji_ssh::SshSession;

use crate::env_resolution::ResolvedEnvironment;
use crate::proxy_routes::RouteTarget;
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
    Deployed {
        deployment_id: String,
        /// The address this endpoint's replica was just leased and deployed to. Authoritative for
        /// this same CLI invocation -- unlike a catalog read on another host, it needs no P2P
        /// replication round trip to be correct, so callers building a cross-host route (see
        /// `proxy_routes::reconcile_catalog_routes`) should prefer it over a catalog-derived
        /// address for this same replica.
        address: std::net::Ipv4Addr,
    },
    Failed {
        error: String,
    },
    SkippedAfterSiblingFailure,
}

pub async fn deploy_endpoint(ctx: &EndpointDeploymentContext<'_>) -> EndpointOutcome {
    let result = match ctx.service.network_mode_dependency() {
        Some(upstream_service_name) => deploy_shared_endpoint(ctx, upstream_service_name).await,
        None => deploy_dynamic_endpoint(ctx).await,
    };
    match result {
        Ok((deployment_id, address)) => EndpointOutcome::Deployed {
            deployment_id,
            address,
        },
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

    // Resolved by service+server, not by recomputing the upstream's replica_id through placement
    // arithmetic: the upstream may use a different placement policy/ordinal scheme than this
    // dependent, but at most one of its replicas can ever be Active/Healthy on any given server
    // (only one container can hold one address), so filtering the catalog directly is both
    // simpler and correct regardless of how the upstream was placed.
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

    report_progress(ctx, "starting candidate");
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
    if let Err(error) =
        health_check::wait_until_healthy(ctx.session, ctx.engine, &candidate_name, &health_plan)
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
        return Err(error.into());
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
        let _ = container_ops::stop(ctx.session, ctx.engine, &old_name).await;
        container_ops::remove_if_present(ctx.session, ctx.engine, &old_name).await?;
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
    sweep_stuck_draining_records(ctx, upstream_service_name).await;
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

    let mut targets = proxy_routes::targets_for_address(
        project,
        ctx.service_name,
        ctx.service.proxy.as_ref(),
        address,
    );
    let other_addresses = other_healthy_addresses(&catalog, ctx.service_name, &replica_id);
    for target in &mut targets {
        target.additional_addresses = other_addresses.clone();
    }
    commit_catalog(
        ctx,
        &replica_id,
        &deployment_id,
        address,
        targets.iter().map(|target| target.port as u16).collect(),
        DeploymentState::Candidate,
        HealthState::Unknown,
    )
    .await?;

    report_progress(ctx, "starting candidate");
    if let Err(error) = container_ops::create_and_start(ctx.session, &run).await {
        release_candidate(ctx, &replica_id, &deployment_id, address, &candidate_name).await;
        restore_stop_first(ctx, previous.as_ref()).await;
        return Err(error);
    }

    let (health_port, health_config) = targets
        .first()
        .map(|target| (target.port, target.healthcheck.as_ref()))
        .unwrap_or((0, None));
    let health_plan = health_check::plan_for_candidate(
        ctx.engine,
        &candidate_name,
        address,
        health_port,
        health_config,
    );
    report_progress(
        ctx,
        &health_check_progress_detail(health_plan.deploy_timeout),
    );
    if let Err(error) =
        health_check::wait_until_healthy(ctx.session, ctx.engine, &candidate_name, &health_plan)
            .await
    {
        release_candidate(ctx, &replica_id, &deployment_id, address, &candidate_name).await;
        restore_stop_first(ctx, previous.as_ref()).await;
        return Err(error.into());
    }

    if !ctx.skip_proxy {
        // Re-read the catalog now, immediately before activation, instead of reusing the snapshot
        // taken at the very start of this function (before the candidate was even created). This
        // matters because a sibling replica's own deploy runs concurrently on another host:
        // confirmed live, the snapshot taken here could be seconds to a minute stale by the time
        // proxy activation actually runs (after image pull, container start, and health-check), so
        // it could still name a sibling's already-superseded, already-torn-down address -- kamal-
        // proxy would then dial a dead target and fail its own health check with "no route to
        // host", well before this endpoint's transaction ever reaches the CLI's later
        // `reconcile_catalog_routes` cross-host reconciliation pass that would have corrected it.
        if let Ok(refreshed_catalog) = crate::agent_client::catalog(ctx.session, project).await {
            let refreshed_addresses =
                other_healthy_addresses(&refreshed_catalog, ctx.service_name, &replica_id);
            for target in &mut targets {
                target.additional_addresses = refreshed_addresses.clone();
            }
        }
        report_progress(ctx, &proxy_activation_progress_detail(&targets));
        let mut activation_result = activate_proxy_routes(ctx, &targets).await;
        if activation_result.is_err() {
            // The pre-activation re-read above narrows the race but doesn't close it: the sibling
            // replica's own concurrent deploy can still commit its new address in the moment
            // between that re-read and kamal-proxy's actual dial. By the time a first activation
            // attempt has exhausted its own health-check timeout (up to `deploy_timeout`), the
            // sibling's concurrent deploy -- going through the same create/health-check sequence --
            // has almost certainly finished by then, so one retry with a freshly re-read catalog
            // recovers cleanly instead of failing the whole deploy over a now-stale target.
            if let Ok(refreshed_catalog) = crate::agent_client::catalog(ctx.session, project).await
            {
                let refreshed_addresses =
                    other_healthy_addresses(&refreshed_catalog, ctx.service_name, &replica_id);
                for target in &mut targets {
                    target.additional_addresses = refreshed_addresses.clone();
                }
                activation_result = activate_proxy_routes(ctx, &targets).await;
            }
        }
        if let Err(error) = activation_result {
            if let Some(previous) = &previous {
                for target in proxy_routes::targets_for_address(
                    project,
                    ctx.service_name,
                    ctx.service.proxy.as_ref(),
                    previous.address,
                ) {
                    let _ = proxy_routes::deploy_route(ctx.session, ctx.engine, &target).await;
                }
            }
            release_candidate(ctx, &replica_id, &deployment_id, address, &candidate_name).await;
            restore_stop_first(ctx, previous.as_ref()).await;
            return Err(error);
        }
    }

    commit_catalog(
        ctx,
        &replica_id,
        &deployment_id,
        address,
        targets.iter().map(|target| target.port as u16).collect(),
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
            sweep_stuck_draining_records(ctx, ctx.service_name).await;
            return Ok((deployment_id, address));
        }
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
    sweep_stuck_draining_records(ctx, ctx.service_name).await;
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
async fn sweep_stuck_draining_records(ctx: &EndpointDeploymentContext<'_>, service_name: &str) {
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

fn report_progress(ctx: &EndpointDeploymentContext<'_>, detail: &str) {
    if let Some(progress) = &ctx.progress {
        progress(&ctx.endpoint.identity, detail);
    }
}

fn health_check_progress_detail(timeout: std::time::Duration) -> String {
    format!("waiting for health check (up to {}s)", timeout.as_secs())
}

/// Addresses of every other currently Active/Healthy replica of `service_name`, for admitting
/// cross-host replicas into this endpoint's own kamal-proxy route alongside its own address.
fn other_healthy_addresses(
    catalog: &[CatalogRecord],
    service_name: &str,
    replica_id: &str,
) -> Vec<std::net::Ipv4Addr> {
    catalog
        .iter()
        .filter(|record| {
            record.service == service_name
                && record.replica_id != replica_id
                && record.state == DeploymentState::Active
                && record.health == HealthState::Healthy
        })
        .map(|record| record.address)
        .collect()
}

fn proxy_activation_progress_detail(targets: &[RouteTarget]) -> String {
    let timeout = targets
        .iter()
        .filter_map(|target| target.healthcheck.as_ref())
        .filter_map(|check| check.deploy_timeout.as_deref())
        .filter_map(health_check::parse_duration)
        .max();
    match timeout {
        Some(timeout) => format!(
            "configuring proxy route (health timeout {}s)",
            timeout.as_secs()
        ),
        None => "configuring proxy route".to_string(),
    }
}

async fn activate_proxy_routes(
    ctx: &EndpointDeploymentContext<'_>,
    candidate_targets: &[RouteTarget],
) -> anyhow::Result<()> {
    for target in candidate_targets {
        proxy_routes::deploy_route(ctx.session, ctx.engine, target).await?;
        proxy_routes::verify_route(ctx.session, ctx.engine, &target.route_name).await?;
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
    use std::net::Ipv4Addr;
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
            "waiting for health check (up to 90s)"
        );
    }

    #[test]
    fn proxy_progress_distinguishes_its_second_health_gate() {
        let targets = vec![RouteTarget {
            route_name: "demo-web-3000".to_string(),
            address: Ipv4Addr::LOCALHOST,
            additional_addresses: Vec::new(),
            port: 3000,
            hosts: vec![],
            tls: false,
            path_prefix: None,
            healthcheck: Some(
                serde_yaml::from_str::<HealthcheckConfig>("deploy_timeout: 60s\n").unwrap(),
            ),
        }];

        assert_eq!(
            proxy_activation_progress_detail(&targets),
            "configuring proxy route (health timeout 60s)"
        );
    }
}
