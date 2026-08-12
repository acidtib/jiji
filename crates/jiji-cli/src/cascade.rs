//! `network_mode: service:<upstream>` cascade logic shared by `jiji deploy`, `jiji service
//! restart`, and `jiji service rollback`: expanding a selection to include a redeployed upstream's
//! dependents, and sequencing a dependent strictly after its upstream within the same invocation.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use jiji_config::Config;
use jiji_network::{NetworkPlan, ServiceEndpointPlan};
use jiji_ssh::SshSession;

use crate::deploy_transaction::{
    deploy_endpoint, EndpointDeploymentContext, EndpointOutcome, EndpointProgress,
};

/// Expands `selected` to include every `network_mode: service:<upstream>` dependent of a service
/// already selected, on whichever of the upstream's own selected servers the dependent is also
/// configured for -- so redeploying an upstream always redeploys its dependents too in the same
/// invocation (their old container becomes network-orphaned once the upstream's old container is
/// torn down). Selecting a dependent by itself never triggers this: only redeploying its upstream
/// can orphan it. Validation already forbids chains (a referenced upstream can't itself be a
/// dependent), so this never needs to recurse.
///
/// A dependent's own `replicas`/`placement` policy is deliberately not used for this: validation
/// already forces a dependent's `replicas` to exactly 1 (`NON_BRIDGE_SCALE`), and its real
/// cardinality is "one instance per shared-namespace server", not an independently round-robined
/// count. `placement::endpoint_replica_id` (sorted-position-in-`servers` ordinal) already
/// expresses exactly that one-per-eligible-server model.
pub(crate) fn add_cascaded_dependents(
    config: &Config,
    plan: &NetworkPlan,
    selected: &mut Vec<ServiceEndpointPlan>,
    replica_ids: &mut BTreeMap<String, String>,
) -> anyhow::Result<()> {
    let mut upstream_servers: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for endpoint in selected.iter() {
        upstream_servers
            .entry(endpoint.service.clone())
            .or_default()
            .insert(endpoint.server.clone());
    }
    let already_selected: BTreeSet<String> = selected.iter().map(|e| e.service.clone()).collect();

    for (dependent_name, dependent_service) in &config.services {
        let Some(upstream_name) = dependent_service.network_mode_dependency() else {
            continue;
        };
        if already_selected.contains(dependent_name.as_str()) {
            continue;
        }
        let Some(servers) = upstream_servers.get(upstream_name) else {
            continue;
        };
        for server_name in servers {
            if !dependent_service.servers.iter().any(|s| s == server_name) {
                continue;
            }
            let replica_id = crate::placement::endpoint_replica_id(
                &config.project,
                dependent_name,
                dependent_service,
                server_name,
            )?;
            let mut endpoint = plan
                .endpoints
                .values()
                .find(|endpoint| {
                    endpoint.service == *dependent_name && endpoint.server == *server_name
                })
                .cloned()
                .expect("cascaded dependent uses a configured endpoint");
            endpoint.identity = format!("{}:{}:{}", config.project, dependent_name, replica_id);
            replica_ids.insert(endpoint.identity.clone(), replica_id);
            selected.push(endpoint);
        }
    }
    selected.sort_by(|a, b| a.identity.cmp(&b.identity));
    Ok(())
}

/// Splits `endpoints_by_service` into two waves: `wave_two` holds every service that is itself a
/// `network_mode: service:<upstream>` dependent whose upstream is *also* present in this same
/// call's selection (must be dispatched strictly after that upstream, since a dependent's old
/// container becomes network-orphaned once the upstream's old container is torn down); `wave_one`
/// holds everything else, including such an upstream itself. Also returns `dependents_of` (keyed
/// by upstream service name), so a caller can both force-skip that upstream's own inline proxy
/// activation and later compute `presumed_failed` for its dependents from the first wave's result.
///
/// This can't be expressed as an in-closure wait on the upstream's completion: every
/// `SshPool::execute_concurrent` task acquires its semaphore permit *before* running, so a
/// dependent blocked waiting inside its own task would hold a permit the upstream needs to ever
/// start, deadlocking outright whenever the pool is bounded to 1 (always true here, since
/// gluetun-shaped upstreams configure `proxy:`). Two separate, sequential `execute_concurrent`
/// calls -- one wave for everything else, one for dependents -- avoids this entirely: by the time
/// the second wave's tasks are even spawned, the first wave has already fully returned and
/// released every permit.
#[allow(clippy::type_complexity)]
pub(crate) fn compute_service_waves(
    config: &Config,
    endpoints_by_service: BTreeMap<String, Vec<ServiceEndpointPlan>>,
) -> (
    BTreeMap<String, Vec<String>>,
    BTreeMap<String, Vec<ServiceEndpointPlan>>,
    BTreeMap<String, Vec<ServiceEndpointPlan>>,
) {
    let mut dependents_of: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for service_name in endpoints_by_service.keys() {
        if let Some(upstream_name) = config.services[service_name].network_mode_dependency() {
            if endpoints_by_service.contains_key(upstream_name) {
                dependents_of
                    .entry(upstream_name.to_string())
                    .or_default()
                    .push(service_name.clone());
            }
        }
    }
    let (wave_two, wave_one): (BTreeMap<_, _>, BTreeMap<_, _>) =
        endpoints_by_service.into_iter().partition(|(name, _)| {
            config.services[name]
                .network_mode_dependency()
                .is_some_and(|upstream| dependents_of.contains_key(upstream))
        });
    (dependents_of, wave_one, wave_two)
}

/// Deploys every endpoint of one service in order, short-circuiting later ones after an earlier
/// one fails (`sibling_failed`, seeded from `presumed_failed` -- true when this service is a
/// `network_mode: service:<upstream>` dependent whose upstream, deployed in an earlier wave, did
/// not fully succeed). Returns its own service name alongside the outcomes so callers can compute
/// per-service success (e.g. an upstream's `dependents_of` lookup) without parsing it back out of
/// `identity`. `images` is keyed by endpoint identity, not service name, since a restart resolves
/// each replica's currently-running image independently (different replicas of the same service
/// can in principle have diverged); a deploy/rollback caller that resolves one image per service
/// just inserts the same value for every identity.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn deploy_service_endpoints(
    sessions: BTreeMap<String, Arc<SshSession>>,
    plan: NetworkPlan,
    replica_ids: BTreeMap<String, String>,
    service_name: String,
    service: jiji_config::Service,
    images: BTreeMap<String, String>,
    resolved_env: crate::env_resolution::ResolvedEnvironment,
    project_root: std::path::PathBuf,
    engine: jiji_config::ContainerEngine,
    skip_proxy: bool,
    endpoints: Vec<ServiceEndpointPlan>,
    progress: Option<jiji_tui::DeployProgressHandle>,
    presumed_failed: bool,
) -> (String, Vec<(String, EndpointOutcome)>) {
    let endpoint_progress: Option<EndpointProgress> = progress.as_ref().map(|handle| {
        let handle = handle.clone();
        Arc::new(move |identity: &str, detail: &str| {
            handle.set_status(identity, detail);
        }) as EndpointProgress
    });
    let mut outcomes = Vec::new();
    let mut sibling_failed = presumed_failed;
    for endpoint in &endpoints {
        if sibling_failed {
            if let Some(handle) = &progress {
                handle.set_status(&endpoint.identity, "skipped — sibling failed");
                handle.mark_skipped(&endpoint.identity);
            }
            outcomes.push((
                endpoint.identity.clone(),
                EndpointOutcome::SkippedAfterSiblingFailure,
            ));
            continue;
        }
        let session = sessions.get(&endpoint.server).expect("connected above");
        let server = &plan.servers[&endpoint.server];
        let replica_id = replica_ids
            .get(&endpoint.identity)
            .expect("selected replica has an identity");
        let image = images
            .get(&endpoint.identity)
            .expect("image resolved for every selected endpoint");
        let ctx = EndpointDeploymentContext {
            session,
            plan: &plan,
            server,
            endpoint,
            service_name: &service_name,
            replica_id,
            service: &service,
            engine,
            image,
            resolved_env: &resolved_env,
            project_root: &project_root,
            skip_proxy,
            max_dir_upload_bytes: crate::commands::deploy::DEFAULT_MAX_DIR_UPLOAD_BYTES,
            progress: endpoint_progress.clone(),
        };
        let outcome = deploy_endpoint(&ctx).await;
        if let Some(handle) = &progress {
            match &outcome {
                EndpointOutcome::Deployed { deployment_id } => {
                    let short = &deployment_id[..12.min(deployment_id.len())];
                    handle.mark_success(&endpoint.identity, &format!("deployed ({short})"));
                }
                EndpointOutcome::Failed { error } => {
                    handle.mark_failed(&endpoint.identity, error);
                }
                EndpointOutcome::SkippedAfterSiblingFailure => {
                    handle.mark_skipped(&endpoint.identity);
                }
            }
        }
        if !matches!(outcome, EndpointOutcome::Deployed { .. }) {
            sibling_failed = true;
        }
        outcomes.push((endpoint.identity.clone(), outcome));
    }
    (service_name, outcomes)
}
