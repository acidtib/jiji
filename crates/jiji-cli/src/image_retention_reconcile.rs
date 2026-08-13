//! Pushes each build-configured service's image-retention spec to every host in its `servers:`
//! set after a successful deploy, so `jiji-agent`'s own reconcile loop
//! (`local_reconcile::reconcile_image_retention`) can prune old image tags continuously instead
//! of only when an operator runs `jiji service prune` by hand.
//!
//! Shape rhymes with `cron_reconcile.rs`'s `reconcile_after_deploy`, but the domain doesn't:
//! cron has a single mobile *owner* per job that `scale` can relocate, while a retention spec is
//! pushed identically, unconditionally, to every host in a service's eligible `servers:` set,
//! because each host prunes its own independent local image cache. There is accordingly no
//! `scale` hook here (nothing about scale changes what needs pushing) and no per-service ownership
//! resolution -- just `resolve_sessions`/`close_newly_opened`, reused from `cron_reconcile.rs`
//! rather than duplicated.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use jiji_agent::api::{RequestBody, ResponseBody};
use jiji_config::{Config, Service, Ssh};
use jiji_network::ServiceEndpointPlan;
use jiji_ssh::SshSession;
use tracing::warn;

use crate::cron_reconcile::{close_newly_opened, fully_deployed_services, resolve_sessions};
use crate::deploy_transaction::EndpointOutcome;
use crate::registry;
use crate::version_requirements::{check_min_version, MIN_RETENTION_AGENT_VERSION};

/// Reconciles image-retention specs for every service in `results` whose every selected endpoint
/// deployed successfully -- the shared post-processing step `jiji deploy`, `jiji service restart`,
/// and `jiji service rollback` all call after their own endpoint deployment completes, alongside
/// the equivalent `cron_reconcile::reconcile_after_deploy` call. Also sweeps every server in the
/// project for a stale spec left by a service that dropped `build:`, changed its `servers:` list,
/// or was renamed/deleted outright (see `sweep_stale_retention_specs`).
pub(crate) async fn reconcile_after_deploy(
    ssh: &Ssh,
    config: &Config,
    selected: &[ServiceEndpointPlan],
    results: &[Vec<(String, EndpointOutcome)>],
    sessions: &BTreeMap<String, Arc<SshSession>>,
) -> Vec<String> {
    let mut problems = Vec::new();
    // The Health RPC behind agent_supports_retention is cached per host for the whole call:
    // a host running several build services would otherwise be version-probed once per service.
    let mut version_cache: BTreeMap<String, bool> = BTreeMap::new();
    for service_name in services_to_reconcile(config, selected, results) {
        let service = &config.services[service_name];
        problems.extend(
            push_retention_spec(
                ssh,
                config,
                service_name,
                service,
                sessions,
                &mut version_cache,
            )
            .await,
        );
    }
    problems.extend(sweep_stale_retention_specs(ssh, config, sessions).await);
    problems
}

/// `true` once `session`'s agent reports a version at or above `MIN_RETENTION_AGENT_VERSION`.
/// Fails open (`true`) on an unparseable/unreachable version, matching
/// `version_requirements::check_min_version`'s own precedent -- the actual RPC attempt right
/// after this call surfaces a real connectivity problem on its own terms, and this function's
/// only job is to keep a known-too-old agent from being sent a request it cannot parse at all.
async fn agent_supports_retention(session: &SshSession, project: &str, server_name: &str) -> bool {
    let Ok(ResponseBody::Health { version, .. }) =
        crate::agent_client::call(session, project, None, RequestBody::Health).await
    else {
        return true;
    };
    match check_min_version(
        "jiji-agent",
        server_name,
        &version,
        MIN_RETENTION_AGENT_VERSION,
        "Run `jiji server upgrade` to enable it here.",
    ) {
        Ok(()) => true,
        Err(error) => {
            warn!(server = %server_name, %error, "image-retention reconcile: skipping this host");
            false
        }
    }
}

/// Every service among `selected` whose every endpoint in `results` deployed successfully, and
/// that has `build:` configured -- only jiji-built image tags are jiji's to prune, matching
/// `jiji service prune`'s existing exclusion of a static `image:` service
/// (`commands/service/prune.rs`). The success fold itself is `cron_reconcile`'s shared
/// `fully_deployed_services`.
fn services_to_reconcile<'a>(
    config: &'a Config,
    selected: &'a [ServiceEndpointPlan],
    results: &[Vec<(String, EndpointOutcome)>],
) -> Vec<&'a str> {
    fully_deployed_services(selected, results)
        .into_iter()
        .filter(|(_, succeeded)| *succeeded)
        .filter_map(|(service_name, _)| {
            let service = config.services.get(service_name)?;
            service.build.is_some().then_some(service_name)
        })
        .collect()
}

async fn push_retention_spec(
    ssh: &Ssh,
    config: &Config,
    service_name: &str,
    service: &Service,
    sessions: &BTreeMap<String, Arc<SshSession>>,
    version_cache: &mut BTreeMap<String, bool>,
) -> Vec<String> {
    let mut problems = Vec::new();
    // An anonymous-pull-capable registry (no `builder.registry.username`) is a legal
    // configuration for a namespaced host; failing to compute a repo string for retention's own
    // sake must not turn an already-successful deploy into a reported failure. Skip retention for
    // this service instead of erroring.
    let repo = match registry::repo_reference(
        &config.builder.registry,
        &config.project,
        service_name,
    ) {
        Ok(repo) => repo,
        Err(error) => {
            warn!(
                service = %service_name, %error,
                "image-retention reconcile: could not compute image repository, skipping retention for this service"
            );
            return problems;
        }
    };
    // Every server in the service's `servers:` set, reusing this command's own sessions and
    // best-effort connecting to the rest: lenient mode so a host outside a `-H`-scoped
    // command's targets that can't be reached is skipped with a log line, never a hard failure.
    let (resolved, newly_opened) = match resolve_sessions(
        ssh,
        config,
        &service.servers,
        sessions,
        Some("image-retention reconcile"),
    )
    .await
    {
        Ok(resolved) => resolved,
        Err(error) => {
            problems.push(format!(
                    "service '{service_name}': could not reach every eligible server to push its image-retention spec: {error}"
                ));
            return problems;
        }
    };
    for (server_name, session) in &resolved {
        let supported = match version_cache.get(server_name) {
            Some(supported) => *supported,
            None => {
                let supported =
                    agent_supports_retention(session, &config.project, server_name).await;
                version_cache.insert(server_name.clone(), supported);
                supported
            }
        };
        if !supported {
            continue;
        }
        let request = RequestBody::ImageRetentionApply {
            service: service_name.to_string(),
            repo: repo.clone(),
            retain: service.retain,
        };
        // Always a reported problem, never downgraded to a warn for a host outside this
        // command's own `sessions`: `resolve_sessions` above already turned "could not connect at
        // all" into a lenient skip for such a host, and `agent_supports_retention` already turned
        // "too old to understand this request" into a lenient skip too, so a failure reaching
        // here means the host was actually connected to and did understand the request, yet the
        // push still failed -- worth surfacing regardless of whether `-H` happened to target it.
        if let Err(error) = crate::agent_client::call(session, &config.project, None, request).await
        {
            problems.push(format!(
                "service '{service_name}': could not push its image-retention spec to '{server_name}': {error}"
            ));
        }
    }
    close_newly_opened(&resolved, &newly_opened).await;
    problems
}

/// Every `(server, service)` pair that should currently have an installed retention spec,
/// computed purely from `config`: any service with `build:` configured, for every server in its
/// `servers:` list. Pure function so the drift logic in `sweep_stale_retention_specs` is testable
/// without SSH.
fn desired_retention_pairs(config: &Config) -> BTreeMap<&str, BTreeSet<&str>> {
    let mut desired: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (service_name, service) in &config.services {
        if service.build.is_none() {
            continue;
        }
        for server_name in &service.servers {
            desired
                .entry(server_name.as_str())
                .or_default()
                .insert(service_name.as_str());
        }
    }
    desired
}

/// Removes an installed spec from any server whose service no longer wants one there: `build:`
/// was dropped, the host was removed from `servers:`, or the service was renamed or deleted
/// outright (its old name is then simply absent from `config.services`, so it can never appear in
/// `desired_retention_pairs`, and any spec still installed under that name is caught here).
/// Reaches every server any *currently* build-configured service's `servers:` list references
/// (`desired_retention_pairs`'s own keys), not just this command's `-H`-scoped `sessions`: a host
/// dropped from `-H` targeting while still part of some other build-configured service's
/// `servers:` would otherwise never be swept again, the same class of bug the cron sweep had
/// before it was fixed (see `AGENTS.md`'s "Scheduled Cron Execution" section). Deliberately not
/// widened to every server in the project the way `push_retention_spec`/`cron_reconcile.rs`
/// widen to one service's own `servers:` list: a server no build-configured service's `servers:`
/// currently references at all is genuinely unrelated to image retention, and dialing it
/// unprompted is exactly what `service_restart_test.rs`'s
/// `restart_without_hosts_filter_never_contacts_an_unrelated_server` guards against project-wide.
/// Lenient: an unreachable server outside this command's own targets is skipped with a warning,
/// never turned into a hard problem for the whole command. A `ImageRetentionList`/`Remove`
/// failure is always a soft skip too, never a hard problem, on the assumption it's usually caused
/// by the same too-old-agent case `agent_supports_retention` already tolerates for pushes -- an
/// agent that understood the request well enough to answer `ImageRetentionList` but then
/// genuinely failed to remove a stale entry is the one case this still surfaces as a problem.
async fn sweep_stale_retention_specs(
    ssh: &Ssh,
    config: &Config,
    sessions: &BTreeMap<String, Arc<SshSession>>,
) -> Vec<String> {
    let mut problems = Vec::new();
    let desired = desired_retention_pairs(config);
    let sweep_targets: Vec<String> = sessions
        .keys()
        .cloned()
        .chain(desired.keys().map(|name| name.to_string()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let (resolved, newly_opened) = match resolve_sessions(
        ssh,
        config,
        &sweep_targets,
        sessions,
        Some("image-retention sweep"),
    )
    .await
    {
        Ok(resolved) => resolved,
        Err(error) => {
            problems.push(format!(
                "could not reach every server to sweep stale image-retention specs: {error}"
            ));
            return problems;
        }
    };
    let empty = BTreeSet::new();
    for (server_name, session) in &resolved {
        let wanted = desired.get(server_name.as_str()).unwrap_or(&empty);
        let installed = match crate::agent_client::call(
            session,
            &config.project,
            None,
            RequestBody::ImageRetentionList,
        )
        .await
        {
            Ok(ResponseBody::ImageRetentionSpecs { specs }) => specs,
            Ok(response) => {
                warn!(
                    server = %server_name, ?response,
                    "image-retention reconcile: agent returned an unexpected response while listing specs for sweep, skipping"
                );
                continue;
            }
            Err(error) => {
                warn!(
                    server = %server_name, %error,
                    "image-retention reconcile: could not list installed specs for sweep, skipping"
                );
                continue;
            }
        };
        for spec in installed {
            if wanted.contains(spec.service.as_str()) {
                continue;
            }
            if let Err(error) = crate::agent_client::call(
                session,
                &config.project,
                None,
                RequestBody::ImageRetentionRemove {
                    service: spec.service.clone(),
                },
            )
            .await
            {
                problems.push(format!(
                    "'{server_name}': could not remove stale image-retention spec for service '{}': {error}",
                    spec.service
                ));
            }
        }
    }
    close_newly_opened(&resolved, &newly_opened).await;
    problems
}

/// `jiji service remove`'s image-retention cleanup: unconditional removal from every eligible
/// server, mirroring `cron_reconcile::remove_all_cron_specs`. Harmless to call for a service that
/// never had a spec installed (a static `image:` service, or one whose deploy never succeeded):
/// the agent's `ImageRetentionRemove` simply reports `removed: false`.
pub(crate) async fn remove_all_retention_specs(
    ssh: &Ssh,
    config: &Config,
    service_name: &str,
    service: &Service,
    sessions: &BTreeMap<String, Arc<SshSession>>,
) -> Vec<String> {
    let mut problems = Vec::new();
    let (resolved, newly_opened) = match resolve_sessions(
        ssh,
        config,
        &service.servers,
        sessions,
        None,
    )
    .await
    {
        Ok(resolved) => resolved,
        Err(error) => {
            problems.push(format!(
                    "service '{service_name}': could not reach every eligible server to remove its image-retention spec: {error}"
                ));
            return problems;
        }
    };
    for (server_name, session) in &resolved {
        if let Err(error) = crate::agent_client::call(
            session,
            &config.project,
            None,
            RequestBody::ImageRetentionRemove {
                service: service_name.to_string(),
            },
        )
        .await
        {
            problems.push(format!(
                "service '{service_name}': could not remove its image-retention spec from '{server_name}': {error}"
            ));
        }
    }
    close_newly_opened(&resolved, &newly_opened).await;
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(identity: &str, service: &str) -> ServiceEndpointPlan {
        ServiceEndpointPlan {
            identity: identity.to_string(),
            project: "demo".to_string(),
            service: service.to_string(),
            server: "app1".to_string(),
        }
    }

    fn config() -> Config {
        serde_yaml::from_str(
            r#"
project: demo
builder:
  engine: docker
servers:
  app1:
    host: 10.0.0.1
services:
  web:
    build: .
    servers: [app1]
  worker:
    build: .
    servers: [app1]
  cache:
    image: redis:7
    servers: [app1]
"#,
        )
        .unwrap()
    }

    #[test]
    fn only_a_fully_successful_service_is_reconciled() {
        let config = config();
        let selected = vec![endpoint("web-1", "web"), endpoint("web-2", "web")];
        let results = vec![vec![
            (
                "web-1".to_string(),
                EndpointOutcome::Deployed {
                    deployment_id: "d1".to_string(),
                },
            ),
            (
                "web-2".to_string(),
                EndpointOutcome::Failed {
                    error: "boom".to_string(),
                },
            ),
        ]];
        assert!(services_to_reconcile(&config, &selected, &results).is_empty());
    }

    #[test]
    fn a_static_image_service_is_excluded_even_when_fully_successful() {
        let config = config();
        let selected = vec![endpoint("cache-1", "cache")];
        let results = vec![vec![(
            "cache-1".to_string(),
            EndpointOutcome::Deployed {
                deployment_id: "d1".to_string(),
            },
        )]];
        assert!(services_to_reconcile(&config, &selected, &results).is_empty());
    }

    #[test]
    fn a_fully_successful_build_configured_service_is_reconciled() {
        let config = config();
        let selected = vec![
            endpoint("web-1", "web"),
            endpoint("worker-1", "worker"),
            endpoint("cache-1", "cache"),
        ];
        let results = vec![vec![
            (
                "web-1".to_string(),
                EndpointOutcome::Deployed {
                    deployment_id: "d1".to_string(),
                },
            ),
            (
                "worker-1".to_string(),
                EndpointOutcome::Failed {
                    error: "boom".to_string(),
                },
            ),
            (
                "cache-1".to_string(),
                EndpointOutcome::Deployed {
                    deployment_id: "d2".to_string(),
                },
            ),
        ]];
        assert_eq!(
            services_to_reconcile(&config, &selected, &results),
            vec!["web"]
        );
    }

    #[test]
    fn desired_retention_pairs_includes_only_build_configured_services() {
        let config = config();
        let desired = desired_retention_pairs(&config);
        assert_eq!(
            desired.get("app1").cloned().unwrap_or_default(),
            BTreeSet::from(["web", "worker"])
        );
    }

    #[test]
    fn desired_retention_pairs_excludes_a_server_not_in_the_service_list() {
        let config: Config = serde_yaml::from_str(
            r#"
project: demo
builder:
  engine: docker
servers:
  app1:
    host: 10.0.0.1
  app2:
    host: 10.0.0.2
services:
  web:
    build: .
    servers: [app1]
"#,
        )
        .unwrap();
        let desired = desired_retention_pairs(&config);
        assert_eq!(desired.get("app1").cloned().unwrap_or_default().len(), 1);
        assert!(!desired.contains_key("app2"));
    }
}
