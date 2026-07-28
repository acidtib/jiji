use std::path::Path;
use std::sync::Arc;

use jiji_config::{ContainerEngine, Service};
use jiji_network::{NetworkPlan, ServerPlan, ServiceEndpointPlan};
use jiji_ssh::SshSession;

use crate::container_runtime::backend_address;
use crate::env_resolution::ResolvedEnvironment;
use crate::proxy_routes::RouteTarget;
use crate::service_network::PreparedServiceCutover;
use crate::{
    container_ops, container_runtime, health_check, mounts, network_guard, proxy_routes,
    service_network,
};

pub type EndpointProgress = Arc<dyn Fn(&str, &str) + Send + Sync>;

pub struct EndpointDeploymentContext<'a> {
    pub session: &'a SshSession,
    pub plan: &'a NetworkPlan,
    pub server: &'a ServerPlan,
    pub endpoint: &'a ServiceEndpointPlan,
    pub service_name: &'a str,
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
        candidate_slot: jiji_network::BackendSlot,
    },
    Failed {
        error: String,
    },
    SkippedAfterSiblingFailure,
}

pub async fn deploy_endpoint(ctx: &EndpointDeploymentContext<'_>) -> EndpointOutcome {
    match deploy_endpoint_inner(ctx).await {
        Ok(slot) => EndpointOutcome::Deployed {
            candidate_slot: slot,
        },
        Err(error) => EndpointOutcome::Failed {
            error: error.to_string(),
        },
    }
}

async fn deploy_endpoint_inner(
    ctx: &EndpointDeploymentContext<'_>,
) -> anyhow::Result<jiji_network::BackendSlot> {
    let project = &ctx.plan.project;
    let identity = &ctx.endpoint.identity;

    report_progress(ctx, "preparing candidate");
    network_guard::verify_generation(ctx.session, ctx.plan, &ctx.server.name).await?;

    let cutover = service_network::prepare_cutover(ctx.session, ctx.plan, identity).await?;
    let candidate_slot = cutover.candidate_slot();
    let previous_slot = cutover.previous_slot();
    let candidate_name =
        container_runtime::container_name(project, ctx.service_name, candidate_slot);

    // Decision 6: the slot to use always comes from `ActiveSlotState`, never from container
    // names/timestamps. A stopped but still-present active container is unambiguous and can be
    // restored after an interrupted stop-first deployment. A missing active container remains
    // ambiguous and must fail rather than guessing.
    if let Some(slot) = previous_slot {
        let active_name = container_runtime::container_name(project, ctx.service_name, slot);
        match container_ops::inspect_status(ctx.session, ctx.engine, &active_name).await? {
            Some(status) if status == "running" => {}
            Some(_) => restore_interrupted_active_container(ctx, slot, &active_name).await?,
            None => anyhow::bail!(
                "Active container '{active_name}' for endpoint '{identity}' is missing. Refusing to guess deployment state; recreate '{active_name}' manually (or reconcile the VIP mapping) before retrying `jiji deploy`."
            ),
        }
    }

    // Anything already occupying the inactive slot is, by definition, not the serving container
    // -- unconditionally disposable before reuse (covers both a leftover unhealthy candidate and
    // recovery from an interrupted prior deploy).
    if container_ops::inspect_status(ctx.session, ctx.engine, &candidate_name)
        .await?
        .is_some()
    {
        let _ = container_ops::stop(ctx.session, ctx.engine, &candidate_name).await;
        container_ops::remove(ctx.session, ctx.engine, &candidate_name).await?;
    }

    if ctx.service.stop_first {
        if let Some(slot) = previous_slot {
            let active_name = container_runtime::container_name(project, ctx.service_name, slot);
            container_ops::stop(ctx.session, ctx.engine, &active_name).await?;
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

    let run = container_runtime::build_run(
        ctx.engine,
        project,
        ctx.service_name,
        &ctx.server.name,
        ctx.image,
        ctx.endpoint,
        ctx.server,
        candidate_slot,
        ctx.service,
        &mount_args,
        &env_file_path,
    );
    report_progress(ctx, "starting candidate");
    if let Err(error) = container_ops::create_and_start(ctx.session, &run).await {
        // A failed `run` can still leave a stopped/half-created container behind (e.g. Docker
        // allocating the container object before failing to bind a published port). Clean it up
        // immediately rather than relying on the next attempt's leftover-slot disposal.
        anyhow::bail!(
            "{error}{} The previous version is still serving traffic.",
            discard_candidate(ctx, &candidate_name).await
        );
    }

    let candidate_targets = targets_for_slot(ctx, candidate_slot);
    let (health_port, health_config) = candidate_targets
        .first()
        .map(|target| (target.port, target.healthcheck.as_ref()))
        .unwrap_or((0, None));
    let candidate_address = backend_address(ctx.endpoint, candidate_slot);
    let health_plan = health_check::plan_for_candidate(
        ctx.engine,
        &candidate_name,
        candidate_address,
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
        // Before VIP activation: remove the unhealthy candidate, leave the old VIP/proxy/container
        // untouched.
        anyhow::bail!(
            "{error}{} The previous version is still serving traffic.",
            discard_candidate(ctx, &candidate_name).await
        );
    }

    report_progress(ctx, "health check passed, activating service VIP");
    if let Err(error) = service_network::commit_after_health_check(
        ctx.session,
        ctx.plan,
        &cutover,
        &health_plan.command,
    )
    .await
    {
        anyhow::bail!(
            "{error}{} The previous version is still serving traffic.",
            discard_candidate(ctx, &candidate_name).await
        );
    }

    if !ctx.skip_proxy {
        report_progress(ctx, &proxy_activation_progress_detail(&candidate_targets));
        if let Err(error) = activate_proxy_routes(ctx, &candidate_targets).await {
            let cleanup = rollback_after_proxy_failure(
                ctx,
                &cutover,
                previous_slot,
                &candidate_targets,
                &candidate_name,
            )
            .await;
            anyhow::bail!(
                "{error} Rolled back: VIP restored to the previous version.{cleanup} Inspect logs with `{} logs {candidate_name}` before retrying.",
                ctx.engine
            );
        }
        report_progress(ctx, "proxy route active");
    }

    if let Some(slot) = previous_slot {
        report_progress(ctx, "removing previous container");
        let old_name = container_runtime::container_name(project, ctx.service_name, slot);
        let _ = container_ops::stop(ctx.session, ctx.engine, &old_name).await;
        container_ops::remove(ctx.session, ctx.engine, &old_name).await?;
    }

    Ok(candidate_slot)
}

fn report_progress(ctx: &EndpointDeploymentContext<'_>, detail: &str) {
    if let Some(progress) = &ctx.progress {
        progress(&ctx.endpoint.identity, detail);
    }
}

fn health_check_progress_detail(timeout: std::time::Duration) -> String {
    format!("waiting for health check (up to {}s)", timeout.as_secs())
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

fn targets_for_slot(
    ctx: &EndpointDeploymentContext<'_>,
    slot: jiji_network::BackendSlot,
) -> Vec<RouteTarget> {
    proxy_routes::targets_for_service(
        &ctx.plan.project,
        ctx.service_name,
        ctx.service.proxy.as_ref(),
        ctx.endpoint,
        slot,
    )
}

async fn restore_interrupted_active_container(
    ctx: &EndpointDeploymentContext<'_>,
    slot: jiji_network::BackendSlot,
    active_name: &str,
) -> anyhow::Result<()> {
    report_progress(ctx, "restoring interrupted active container");
    container_ops::start(ctx.session, ctx.engine, active_name)
        .await
        .map_err(|error| {
            anyhow::anyhow!(
                "Active container '{active_name}' is stopped and could not be restarted: {error}"
            )
        })?;
    crate::commands::network::bridge::reconcile_podman_dns_address(
        ctx.session,
        ctx.engine,
        &ctx.server.bridge_interface,
        ctx.server.dns_address,
    )
    .await?;

    let active_targets = targets_for_slot(ctx, slot);
    let (health_port, health_config) = active_targets
        .first()
        .map(|target| (target.port, target.healthcheck.as_ref()))
        .unwrap_or((0, None));
    let health_plan = health_check::plan_for_candidate(
        ctx.engine,
        active_name,
        backend_address(ctx.endpoint, slot),
        health_port,
        health_config,
    );
    health_check::wait_until_healthy(ctx.session, ctx.engine, active_name, &health_plan)
        .await
        .map_err(|error| {
            anyhow::anyhow!(
                "Active container '{active_name}' restarted but did not become healthy: {error}"
            )
        })?;

    if !ctx.skip_proxy {
        report_progress(ctx, "restoring active proxy route");
        activate_proxy_routes(ctx, &active_targets)
            .await
            .map_err(|error| {
                anyhow::anyhow!(
                    "Active container '{active_name}' restarted, but its proxy route could not be restored: {error}"
                )
            })?;
    }

    Ok(())
}

/// Stops and removes `candidate_name`, returning a suffix describing the outcome for inclusion in
/// the caller's error message: empty on success, or an actionable manual-removal command if the
/// automatic cleanup itself failed (never silently leaves an orphaned container unmentioned).
async fn discard_candidate(ctx: &EndpointDeploymentContext<'_>, candidate_name: &str) -> String {
    let _ = container_ops::stop(ctx.session, ctx.engine, candidate_name).await;
    match container_ops::remove(ctx.session, ctx.engine, candidate_name).await {
        Ok(()) => format!(" Candidate '{candidate_name}' was removed."),
        Err(error) => format!(
            " Candidate '{candidate_name}' could not be removed automatically ({error}); remove it manually with `{} rm -f {candidate_name}`.",
            ctx.engine
        ),
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

/// Failure after VIP (and possibly partial proxy) activation: restore the previous VIP slot and
/// proxy route when one existed (a replacement deploy), or remove the dangling route entirely
/// when there wasn't one (a first deployment -- there is nothing to restore it to), then remove
/// the candidate. Never silently removes both versions. Returns a suffix describing anything that
/// could not be cleaned up automatically.
async fn rollback_after_proxy_failure(
    ctx: &EndpointDeploymentContext<'_>,
    cutover: &PreparedServiceCutover,
    previous_slot: Option<jiji_network::BackendSlot>,
    candidate_targets: &[RouteTarget],
    candidate_name: &str,
) -> String {
    let mut warnings = String::new();

    if let Err(error) = service_network::rollback_cutover(ctx.session, ctx.plan, cutover).await {
        warnings.push_str(&format!(
            " VIP rollback failed ({error}); reconcile the active-slot mapping manually."
        ));
    }

    match previous_slot {
        Some(slot) => {
            for target in targets_for_slot(ctx, slot) {
                if let Err(error) =
                    proxy_routes::deploy_route(ctx.session, ctx.engine, &target).await
                {
                    warnings.push_str(&format!(
                        " Restoring proxy route '{}' to the previous version failed ({error}); repair it manually.",
                        target.route_name
                    ));
                }
            }
        }
        None => {
            for target in candidate_targets {
                if let Err(error) =
                    proxy_routes::remove_route(ctx.session, ctx.engine, &target.route_name).await
                {
                    warnings.push_str(&format!(
                        " Removing dangling proxy route '{}' failed ({error}); repair it manually.",
                        target.route_name
                    ));
                }
            }
        }
    }

    warnings.push_str(&discard_candidate(ctx, candidate_name).await);
    warnings
}

#[cfg(test)]
mod tests {
    use super::{health_check_progress_detail, proxy_activation_progress_detail, RouteTarget};
    use jiji_config::HealthcheckConfig;
    use std::net::Ipv4Addr;
    use std::time::Duration;

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
