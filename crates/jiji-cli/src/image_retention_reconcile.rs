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

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use jiji_agent::api::RequestBody;
use jiji_config::{Config, Service, Ssh};
use jiji_network::ServiceEndpointPlan;
use jiji_ssh::SshSession;

use crate::cron_reconcile::{close_newly_opened, resolve_sessions};
use crate::deploy_transaction::EndpointOutcome;
use crate::registry;

/// A plain wall-clock second count: unlike `CronJobSpec::revision` (taken from the catalog
/// record's own revision counter), there is no equivalent per-deploy counter here to reuse, and
/// `AgentStore::apply_image_retention_spec` never compares on `revision` for its idempotent-upsert
/// decision (only `repo`/`retain`) -- see the plan's "RPC" section. A wall-clock value is
/// monotonic enough for "last write wins" to stay well-defined without any extra state.
fn current_revision() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Reconciles image-retention specs for every service in `results` whose every selected endpoint
/// deployed successfully -- the shared post-processing step `jiji deploy`, `jiji service restart`,
/// and `jiji service rollback` all call after their own endpoint deployment completes, alongside
/// the equivalent `cron_reconcile::reconcile_after_deploy` call.
pub(crate) async fn reconcile_after_deploy(
    ssh: &Ssh,
    config: &Config,
    selected: &[ServiceEndpointPlan],
    results: &[Vec<(String, EndpointOutcome)>],
    sessions: &BTreeMap<String, Arc<SshSession>>,
) -> Vec<String> {
    let mut problems = Vec::new();
    for service_name in services_to_reconcile(config, selected, results) {
        let service = &config.services[service_name];
        problems.extend(push_retention_spec(ssh, config, service_name, service, sessions).await);
    }
    problems
}

/// Every service among `selected` whose every endpoint in `results` deployed successfully, and
/// that has `build:` configured -- only jiji-built image tags are jiji's to prune, matching
/// `jiji service prune`'s existing exclusion of a static `image:` service
/// (`commands/service/prune.rs`).
fn services_to_reconcile<'a>(
    config: &'a Config,
    selected: &'a [ServiceEndpointPlan],
    results: &[Vec<(String, EndpointOutcome)>],
) -> Vec<&'a str> {
    let identity_to_service: BTreeMap<&str, &str> = selected
        .iter()
        .map(|endpoint| (endpoint.identity.as_str(), endpoint.service.as_str()))
        .collect();
    let mut service_success: BTreeMap<&str, bool> = BTreeMap::new();
    for outcomes in results {
        for (identity, outcome) in outcomes {
            let Some(service_name) = identity_to_service.get(identity.as_str()).copied() else {
                continue;
            };
            let succeeded = matches!(outcome, EndpointOutcome::Deployed { .. });
            service_success
                .entry(service_name)
                .and_modify(|ok| *ok &= succeeded)
                .or_insert(succeeded);
        }
    }
    service_success
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
) -> Vec<String> {
    let mut problems = Vec::new();
    let repo = match registry::repo_reference(
        &config.builder.registry,
        &config.project,
        service_name,
    ) {
        Ok(repo) => repo,
        Err(error) => {
            problems.push(format!(
                "service '{service_name}': could not compute its image repository for retention: {error}"
            ));
            return problems;
        }
    };
    let (resolved, newly_opened) = match resolve_sessions(ssh, config, &service.servers, sessions)
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
    let revision = current_revision();
    for (server_name, session) in &resolved {
        let request = RequestBody::ImageRetentionApply {
            service: service_name.to_string(),
            repo: repo.clone(),
            retain: service.retain,
            revision,
        };
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
    let (resolved, newly_opened) = match resolve_sessions(ssh, config, &service.servers, sessions)
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
    fn current_revision_is_a_plausible_unix_timestamp() {
        // Not a fixed-value assertion (it's wall-clock derived) -- just proves it's a real
        // "recent" second count, not a placeholder like 0.
        assert!(current_revision() > 1_700_000_000);
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
}
