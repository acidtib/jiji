//! Installs/removes cron job specifications on the owning replica's agent
//! (`docs/architecture-notes.md#ownership-and-reconciliation`).
//! Called by `jiji deploy`/`jiji service restart`/`rollback` (via `reconcile_after_deploy`, after
//! every endpoint of a service has deployed successfully, whether or not it currently has
//! `crons:` configured), by `jiji service scale` (ownership may move even without a redeploy),
//! and by `jiji service remove` (unconditional removal, no ownership computation needed).
//!
//! Cron specs are never replicated between agents (see `jiji_agent::cron`'s module doc comment),
//! so finding a stale spec -- left on a former owner after an ownership transfer, left behind
//! because its `crons:` entry was renamed or deleted, or left behind on a host `servers:` itself
//! dropped -- requires actually connecting to every eligible agent and asking what it has
//! installed; the CLI has no other way to discover it and no memory of what used to be
//! configured. `reconcile_service_crons` therefore connects to every server in the *project*
//! (`config.servers`), not just the service's current `servers:` list or whatever the command's
//! `-H`/`-S` filters selected -- a host a service's `servers:` no longer lists is still swept,
//! leniently, since it can still have a stale spec installed -- extending the caller's session
//! map in place, and always sweeps every one of them
//! (`remove_specs_absent_from`) regardless of whether `service.crons` is empty.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::Context;
use jiji_agent::api::{RequestBody, ResponseBody};
use jiji_agent::catalog::{CatalogRecord, DeploymentState, HealthState};
use jiji_agent::cron::CronJobSpec;
use jiji_config::{CommandValue, Config, CronConfig, Service, Ssh};
use jiji_network::{NetworkPlan, ServiceEndpointPlan};
use jiji_ssh::SshSession;
use tracing::warn;

use crate::deploy_transaction::EndpointOutcome;
use crate::placement::ReplicaAssignment;
use crate::{container_runtime, mounts, ssh_adapter};

fn to_agent_overlap(value: jiji_config::CronOverlap) -> jiji_agent::cron::CronOverlap {
    match value {
        jiji_config::CronOverlap::Forbid => jiji_agent::cron::CronOverlap::Forbid,
    }
}

fn to_agent_missed_runs(value: jiji_config::CronMissedRuns) -> jiji_agent::cron::CronMissedRuns {
    match value {
        jiji_config::CronMissedRuns::Skip => jiji_agent::cron::CronMissedRuns::Skip,
    }
}

fn render_command(command: &CommandValue) -> Vec<String> {
    match command {
        CommandValue::Single(value) => vec![value.clone()],
        CommandValue::Multiple(values) => values.clone(),
    }
}

/// Among `assignments`, the lowest-ordinal one whose `replica_id` has an Active/Healthy record in
/// `catalog` right now (the plan's "The CLI selects the active replica with the lowest ordinal as
/// the source and owner").
pub(crate) fn select_cron_owner<'a>(
    assignments: &'a [ReplicaAssignment],
    catalog: &'a [CatalogRecord],
) -> Option<(&'a ReplicaAssignment, &'a CatalogRecord)> {
    assignments
        .iter()
        .filter_map(|assignment| {
            catalog
                .iter()
                .find(|record| {
                    record.replica_id == assignment.replica_id
                        && record.state == DeploymentState::Active
                        && record.health == HealthState::Healthy
                })
                .map(|record| (assignment, record))
        })
        .min_by_key(|(assignment, _)| assignment.ordinal)
}

/// Connects to `service`'s eligible servers (reusing anything already in `sessions`) and resolves
/// its current cron owner: the same lookup `reconcile_service_crons` does before installing, now
/// shared with the read-only `jiji service cron list/status/run/logs` commands (Phase 6). Returns
/// the owner's server name, a session to it, its catalog record, and its placement assignment.
/// `find_owner`'s result: the resolved session map is included so the caller can keep using it
/// for further calls (e.g. `list`/`status` looping over several services) and is responsible for
/// closing `newly_opened` when it's done with all of them (`close_newly_opened`).
pub(crate) struct CronOwner {
    pub server_name: String,
    pub session: Arc<SshSession>,
    pub record: CatalogRecord,
    pub assignment: ReplicaAssignment,
}

pub(crate) async fn find_owner(
    ssh: &Ssh,
    config: &Config,
    service_name: &str,
    service: &Service,
    sessions: &BTreeMap<String, Arc<SshSession>>,
) -> anyhow::Result<(CronOwner, BTreeMap<String, Arc<SshSession>>, Vec<String>)> {
    let (resolved, newly_opened) =
        resolve_sessions(ssh, config, &service.servers, sessions, None).await?;

    let result = find_owner_in(config, service_name, service, &resolved).await;
    match result {
        Ok(owner) => Ok((owner, resolved, newly_opened)),
        Err(error) => {
            close_newly_opened(&resolved, &newly_opened).await;
            Err(error)
        }
    }
}

async fn find_owner_in(
    config: &Config,
    service_name: &str,
    service: &Service,
    sessions: &BTreeMap<String, Arc<SshSession>>,
) -> anyhow::Result<CronOwner> {
    let seed = service
        .servers
        .iter()
        .find_map(|name| sessions.get(name))
        .ok_or_else(|| anyhow::anyhow!("service '{service_name}': no reachable server"))?;
    let catalog = crate::agent_client::catalog(seed, &config.project)
        .await
        .with_context(|| {
            format!("service '{service_name}': could not read the service catalog to determine cron ownership")
        })?;
    let assignments = crate::placement::assignments_for(
        &config.project,
        service_name,
        &service.servers,
        service.scale,
    );
    let Some((owner_assignment, owner_record)) = select_cron_owner(&assignments, &catalog) else {
        anyhow::bail!(
            "service '{service_name}': no active, healthy replica; it has no cron owner right now"
        );
    };
    let server_name = owner_record.owner_node_id.clone();
    let session = sessions.get(&server_name).cloned().ok_or_else(|| {
        anyhow::anyhow!(
            "service '{service_name}': could not reach '{server_name}', the owning host for its cron jobs"
        )
    })?;
    Ok(CronOwner {
        server_name,
        session,
        record: owner_record.clone(),
        assignment: owner_assignment.clone(),
    })
}

/// `.jiji/{project}/env/{service}-{server}.env`, matching `env_resolution::stage_env_file`'s own
/// formula exactly: since the owner replica is Active/Healthy, its own most recent deploy already
/// staged this file at this deterministic, service-scoped (not deployment-id-scoped) path, so a
/// cron run can reuse it verbatim without re-staging or needing `ResolvedEnvironment` at all.
///
/// Relative to the SSH login's home directory, same as `stage_env_file`/`mounts::remote_mount_base`
/// -- callers that hand this (or `mounts::build_all_mount_args`'s bind-mount sources) to
/// `jiji-agent` must run it through `absolutize`/`absolutize_mount_args` first (see their doc
/// comments for why).
pub(crate) fn owner_env_file_path(project: &str, service_name: &str, owner_server: &str) -> String {
    format!(".jiji/{project}/env/{service_name}-{owner_server}.env")
}

/// `jiji-agent` spawns cron containers directly via `tokio::process::Command`, never over SSH, so
/// it has no notion of the SSH login's home directory that `stage_env_file`'s and
/// `mounts::remote_mount_base`'s `.jiji/...`-relative paths implicitly resolve against (confirmed
/// live: a scheduled cron run failed with "no such file or directory" against jiji-agent's own `/`
/// working directory). A cron spec sent to the agent must therefore carry an absolute path.
///
/// `pub(crate)`: also used by `commands/service/cron/list.rs`, which must render the exact same
/// absolute paths to recompute a spec's expected `canonical_hash` for drift detection -- a
/// relative path there would make every installed cron report `drifted` unconditionally, since
/// the actually-installed hash (computed here, from `reconcile_service_crons_inner`) always has
/// the absolute form baked in.
pub(crate) async fn remote_home_dir(session: &SshSession) -> anyhow::Result<String> {
    let result = session.execute("pwd").await.with_context(|| {
        format!(
            "could not determine the home directory on {}",
            session.host()
        )
    })?;
    let home = result.stdout.trim();
    if !result.success || !home.starts_with('/') {
        anyhow::bail!(
            "unexpected `pwd` output on {}: {:?}",
            session.host(),
            result.stderr.trim()
        );
    }
    Ok(home.to_string())
}

pub(crate) fn absolutize(home: &str, relative: &str) -> String {
    format!("{home}/{relative}")
}

/// Only `.jiji/...`-relative bind-mount sources (from `files:`/`directories:`) need rewriting --
/// named volumes and already-absolute host bind mounts (`mounts::build_all_mount_args`'s other two
/// kinds of `-v` argument) pass through unchanged.
pub(crate) fn absolutize_mount_args(home: &str, args: Vec<String>) -> Vec<String> {
    args.into_iter()
        .map(|arg| {
            if arg.starts_with(".jiji/") {
                absolutize(home, &arg)
            } else {
                arg
            }
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_apply_request(
    service_name: &str,
    cron_name: &str,
    cron: &CronConfig,
    image: &str,
    mount_args: &[String],
    resource_args: &[String],
    env_file_path: &str,
    source_deployment_id: &str,
    source_replica_id: &str,
    bridge_network: &str,
    dns_address: std::net::Ipv4Addr,
    revision: u64,
) -> RequestBody {
    let command = render_command(&cron.command);
    let timeout_seconds = cron
        .timeout_duration()
        .unwrap_or(std::time::Duration::from_secs(3600))
        .as_secs();
    let overlap = to_agent_overlap(cron.overlap);
    let missed_runs = to_agent_missed_runs(cron.missed_runs);

    // Computed via the exact same function the agent itself uses for its own idempotent-upsert
    // comparison (`jiji-cli` already depends on `jiji-agent`), so drift detection can never
    // silently diverge between the two crates. Identity/ownership fields are irrelevant to the
    // hash (see `CronJobSpec::canonical_hash`'s doc comment) -- placeholders here are never sent.
    let canonical_hash = CronJobSpec {
        project: String::new(),
        service: service_name.to_string(),
        cron_name: cron_name.to_string(),
        revision,
        canonical_hash: String::new(),
        owner_node_id: String::new(),
        owner_epoch: 0,
        server: String::new(),
        source_deployment_id: source_deployment_id.to_string(),
        source_replica_id: source_replica_id.to_string(),
        image: image.to_string(),
        schedule: cron.schedule.clone(),
        timezone: cron.timezone.clone(),
        timeout_seconds,
        overlap,
        missed_runs,
        command: command.clone(),
        env_file_path: env_file_path.to_string(),
        mount_args: mount_args.to_vec(),
        resource_args: resource_args.to_vec(),
        bridge_network: bridge_network.to_string(),
        dns_address: dns_address.to_string(),
    }
    .canonical_hash();

    RequestBody::CronSpecApply {
        service: service_name.to_string(),
        cron_name: cron_name.to_string(),
        revision,
        canonical_hash,
        source_deployment_id: source_deployment_id.to_string(),
        source_replica_id: source_replica_id.to_string(),
        image: image.to_string(),
        schedule: cron.schedule.clone(),
        timezone: cron.timezone.clone(),
        timeout_seconds,
        overlap,
        missed_runs,
        command,
        env_file_path: env_file_path.to_string(),
        mount_args: mount_args.to_vec(),
        resource_args: resource_args.to_vec(),
        bridge_network: bridge_network.to_string(),
        dns_address: dns_address.to_string(),
    }
}

/// Builds a local session map covering every one of `servers`: an `Arc` clone from `sessions`
/// wherever already connected (the command's own `-H`/`-S`-selected targets), a fresh connection
/// for anything else. Returns the names newly connected here alongside the map so the caller can
/// close exactly those when done -- `sessions` itself is never mutated (this module never holds
/// a long-lived `&mut` on the caller's session pool, since callers invoke this from inside a
/// closure that's often already borrowing it immutably for lock management).
///
/// `lenient` selects the failure mode for servers outside the caller's already-connected set:
/// `None` keeps the strict all-or-nothing behavior (an undefined server or failed connection
/// aborts the whole resolve), `Some(subject)` logs and skips that server instead, so a host a
/// `-H`-scoped command never targeted cannot fail it.
pub(crate) async fn resolve_sessions(
    ssh: &Ssh,
    config: &Config,
    servers: &[String],
    sessions: &BTreeMap<String, Arc<SshSession>>,
    lenient: Option<&str>,
) -> anyhow::Result<(BTreeMap<String, Arc<SshSession>>, Vec<String>)> {
    let strict = lenient.is_none();
    let subject = lenient.unwrap_or("");
    let mut resolved = BTreeMap::new();
    let mut newly_opened = Vec::new();
    for server_name in servers {
        if let Some(session) = sessions.get(server_name) {
            resolved.insert(server_name.clone(), Arc::clone(session));
            continue;
        }
        let named = match config.servers.get(server_name) {
            Some(named) => named,
            None if !strict => {
                warn!(
                    server = %server_name,
                    "{subject}: server is not defined in configuration, skipping it"
                );
                continue;
            }
            None => anyhow::bail!(
                "Server '{server_name}' referenced by cron reconciliation is not defined in configuration"
            ),
        };
        let options = match ssh_adapter::connect_options(server_name, named, ssh) {
            Ok(options) => options,
            Err(error) if !strict => {
                warn!(
                    server = %server_name, %error,
                    "{subject}: could not prepare a connection outside this command's targets, skipping it"
                );
                continue;
            }
            Err(error) => return Err(error),
        };
        let session = match SshSession::connect(&options).await {
            Ok(session) => session,
            Err(error) if !strict => {
                warn!(
                    server = %server_name, %error,
                    "{subject}: could not reach a server outside this command's targets, skipping it without failing the command"
                );
                continue;
            }
            Err(error) => {
                return Err(anyhow::Error::from(error).context(format!(
                    "Could not connect to '{server_name}' to reconcile cron ownership"
                )));
            }
        };
        resolved.insert(server_name.clone(), Arc::new(session));
        newly_opened.push(server_name.clone());
    }
    Ok((resolved, newly_opened))
}

pub(crate) async fn close_newly_opened(
    sessions: &BTreeMap<String, Arc<SshSession>>,
    newly_opened: &[String],
) {
    for name in newly_opened {
        if let Some(session) = sessions.get(name) {
            session.close().await;
        }
    }
}

/// Reconciles every `crons:` entry for one service: installs on the current owner, then sweeps
/// every eligible host (owner included) for an installed spec that no longer belongs there --
/// left by a previous owner after an ownership transfer, left behind because its `crons:` entry
/// was renamed or deleted, or left behind on a host that `servers:` itself dropped (cron specs
/// are never replicated, so a host missing from the service's *current* `servers:` is otherwise
/// permanently unreachable by this sweep even though its stale spec is still installed and still
/// running). Always runs the sweep, even when `service.crons` is now empty (there may still be a
/// spec installed from before the last edit) and even when installation itself failed (a broken
/// owner shouldn't block cleanup on hosts that are still reachable).
///
/// Never returns an error the caller should fail its own command over: a service deployment that
/// already succeeded must not be rolled back for a cron-only failure (Phase 5's "partial failure"
/// requirement). Returns human-readable problems instead, empty on full success; each already
/// tells the operator to redeploy.
pub(crate) async fn reconcile_service_crons(
    ssh: &Ssh,
    config: &Config,
    plan: &NetworkPlan,
    service_name: &str,
    service: &Service,
    sessions: &BTreeMap<String, Arc<SshSession>>,
) -> Vec<String> {
    let mut problems = Vec::new();
    let redeploy_hint = "Run `jiji deploy` again to retry cron installation.";

    let (service_sessions, mut newly_opened) = match resolve_sessions(
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
                    "service '{service_name}': could not reach every eligible server to reconcile cron jobs: {error}. {redeploy_hint}"
                ));
            return problems;
        }
    };

    // Widen the sweep beyond `service.servers` to every server in the project, but only when this
    // service actually has `crons:` configured right now: a service that has never used crons
    // has nothing to sweep, and must never touch a server outside its own `servers:` (see
    // `restart_without_hosts_filter_never_contacts_an_unrelated_server`). A service with
    // `crons:` populated can still have a stale spec on a host `servers:` itself dropped, and
    // `service.servers` is validated to be a subset of `config.servers`, so this only adds hosts
    // that used to be eligible. Lenient, since an unrelated project host being briefly
    // unreachable must not turn into a reported problem for this service.
    let (sweep_sessions, sweep_newly_opened) = if service.crons.is_empty() {
        (service_sessions.clone(), Vec::new())
    } else {
        let sweep_targets: Vec<String> = config.servers.keys().cloned().collect();
        resolve_sessions(
            ssh,
            config,
            &sweep_targets,
            &service_sessions,
            Some("cron sweep"),
        )
        .await
        .unwrap_or_else(|_| (service_sessions.clone(), Vec::new()))
    };
    newly_opened.extend(sweep_newly_opened);

    let result = reconcile_service_crons_inner(
        config,
        plan,
        service_name,
        service,
        &sweep_sessions,
        redeploy_hint,
    )
    .await;
    close_newly_opened(&sweep_sessions, &newly_opened).await;
    result
}

async fn reconcile_service_crons_inner(
    config: &Config,
    plan: &NetworkPlan,
    service_name: &str,
    service: &Service,
    sessions: &BTreeMap<String, Arc<SshSession>>,
    redeploy_hint: &str,
) -> Vec<String> {
    let mut problems = Vec::new();

    let (owner_server_name, installed_on_owner) = if service.crons.is_empty() {
        (None, BTreeSet::new())
    } else {
        let (install_problems, owner_server_name, installed_on_owner) =
            install_on_owner(config, plan, service_name, service, sessions, redeploy_hint).await;
        problems.extend(install_problems);
        (owner_server_name, installed_on_owner)
    };

    // Always sweep every eligible session, regardless of whether the install step above ran or
    // succeeded: the only way to find a spec that disappeared from `crons:` (a rename or a full
    // deletion) is to ask each agent what it actually has installed and compare against the
    // current desired set, since the CLI has no memory of what used to be configured.
    let desired: BTreeSet<&str> = service.crons.keys().map(String::as_str).collect();
    let none_desired = BTreeSet::new();
    for (server_name, session) in sessions {
        // The owner keeps all configured jobs. A former owner keeps a configured job only until
        // that specific job installs successfully on the new owner. This preserves the required
        // install-before-remove order when one apply fails. If ownership could not be resolved,
        // preserve configured names everywhere for this pass. Deleted names remain absent from
        // every set and are removed from every reachable agent.
        let former_owner_desired = desired_on_former_owner(&desired, &installed_on_owner);
        let desired_for_server = match owner_server_name.as_deref() {
            Some(owner) if owner == server_name => &desired,
            Some(_) => &former_owner_desired,
            None if desired.is_empty() => &none_desired,
            None => &desired,
        };
        problems.extend(
            remove_specs_absent_from(
                session,
                &config.project,
                service_name,
                server_name,
                desired_for_server,
            )
            .await,
        );
    }

    problems
}

fn desired_on_former_owner<'a>(
    desired: &BTreeSet<&'a str>,
    installed_on_owner: &BTreeSet<String>,
) -> BTreeSet<&'a str> {
    desired
        .iter()
        .copied()
        .filter(|cron_name| !installed_on_owner.contains(*cron_name))
        .collect()
}

/// Installs/updates every `crons:` entry on the current owner. Split out from
/// `reconcile_service_crons_inner` so that function can skip straight to the stale-spec sweep
/// when `service.crons` is empty, instead of failing here on e.g. "no active, healthy replica"
/// for a service that was never going to install anything in the first place.
async fn install_on_owner(
    config: &Config,
    plan: &NetworkPlan,
    service_name: &str,
    service: &Service,
    sessions: &BTreeMap<String, Arc<SshSession>>,
    redeploy_hint: &str,
) -> (Vec<String>, Option<String>, BTreeSet<String>) {
    let mut problems = Vec::new();
    let mut installed = BTreeSet::new();
    let Some(seed) = service.servers.iter().find_map(|name| sessions.get(name)) else {
        problems.push(format!(
            "service '{service_name}': no reachable server; cron jobs were not reconciled. {redeploy_hint}"
        ));
        return (problems, None, installed);
    };
    let catalog = match crate::agent_client::catalog(seed, &config.project).await {
        Ok(catalog) => catalog,
        Err(error) => {
            problems.push(format!(
                "service '{service_name}': could not read the service catalog to determine cron ownership: {error}. {redeploy_hint}"
            ));
            return (problems, None, installed);
        }
    };
    let assignments = crate::placement::assignments_for(
        &config.project,
        service_name,
        &service.servers,
        service.scale,
    );
    let Some((owner_assignment, owner_record)) = select_cron_owner(&assignments, &catalog) else {
        problems.push(format!(
            "service '{service_name}': no active, healthy replica; cron jobs were not reconciled. {redeploy_hint}"
        ));
        return (problems, None, installed);
    };
    let owner_server_name = owner_record.owner_node_id.clone();
    let (Some(owner_session), Some(owner_server)) = (
        sessions.get(&owner_server_name).cloned(),
        plan.servers.get(&owner_server_name),
    ) else {
        problems.push(format!(
            "service '{service_name}': could not reach '{owner_server_name}', the owning host for its cron jobs. {redeploy_hint}"
        ));
        return (problems, Some(owner_server_name), installed);
    };

    let owner_home = match remote_home_dir(&owner_session).await {
        Ok(home) => home,
        Err(error) => {
            problems.push(format!(
                "service '{service_name}': could not determine the cron owner's home directory on '{owner_server_name}': {error}. {redeploy_hint}"
            ));
            return (problems, Some(owner_server_name), installed);
        }
    };

    let mount_args = match mounts::build_all_mount_args(service, &config.project, service_name) {
        Ok(args) => absolutize_mount_args(&owner_home, args),
        Err(error) => {
            problems.push(format!(
                "service '{service_name}': could not render mount arguments for its cron jobs: {error}. {redeploy_hint}"
            ));
            return (problems, Some(owner_server_name), installed);
        }
    };
    let resource_args = container_runtime::render_resource_options(service);
    let env_file_path = absolutize(
        &owner_home,
        &owner_env_file_path(&config.project, service_name, &owner_server_name),
    );

    for (cron_name, cron) in &service.crons {
        let request = render_apply_request(
            service_name,
            cron_name,
            cron,
            &owner_record.image,
            &mount_args,
            &resource_args,
            &env_file_path,
            &owner_record.deployment_id,
            &owner_assignment.replica_id,
            &owner_server.bridge_name,
            owner_server.dns_address,
            owner_record.revision,
        );
        match crate::agent_client::call(&owner_session, &config.project, None, request).await {
            Ok(ResponseBody::CronSpecApplied { .. }) => {
                installed.insert(cron_name.clone());
            }
            Ok(response) => problems.push(format!(
                "service '{service_name}' cron '{cron_name}': agent on '{owner_server_name}' returned an unexpected response: {response:?}. {redeploy_hint}"
            )),
            Err(error) => problems.push(format!(
                "service '{service_name}' cron '{cron_name}': could not install on '{owner_server_name}': {error}. {redeploy_hint}"
            )),
        }
    }

    (problems, Some(owner_server_name), installed)
}

/// Removes every installed spec for `service_name` on `session` whose `cron_name` isn't in
/// `desired`. `desired` empty removes every installed spec for the service, regardless of
/// `crons:`'s current content (`remove_all_cron_specs`'s case); a non-empty `desired` set removes
/// only what's genuinely absent, leaving an untouched spec's revision/hash exactly as installed
/// (`reconcile_service_crons_inner`'s case, where `install_on_owner` already applied the current
/// set separately). No `redeploy_hint` here: unlike an install failure, a stale-removal failure
/// doesn't need a specific "run this command again" pointer -- the next reconciliation pass (any
/// deploy/restart/rollback/scale) retries it the same way.
async fn remove_specs_absent_from(
    session: &SshSession,
    project: &str,
    service_name: &str,
    server_name: &str,
    desired: &BTreeSet<&str>,
) -> Vec<String> {
    let mut problems = Vec::new();
    let installed = match crate::agent_client::call(
        session,
        project,
        None,
        RequestBody::CronSpecList,
    )
    .await
    {
        Ok(ResponseBody::CronSpecs { specs }) => specs,
        Ok(_) => Vec::new(),
        Err(error) => {
            problems.push(format!(
                "service '{service_name}': could not list installed cron jobs on '{server_name}': {error}"
            ));
            return problems;
        }
    };
    for spec in installed
        .iter()
        .filter(|spec| spec.service == service_name && !desired.contains(spec.cron_name.as_str()))
    {
        if let Err(error) = crate::agent_client::call(
            session,
            project,
            None,
            RequestBody::CronSpecRemove {
                service: service_name.to_string(),
                cron_name: spec.cron_name.clone(),
            },
        )
        .await
        {
            problems.push(format!(
                "service '{service_name}' cron '{}': could not remove a stale installation on '{server_name}': {error}",
                spec.cron_name
            ));
        }
    }
    problems
}

/// Every service among `selected` whose every endpoint in `results` deployed successfully, as a
/// name -> all-succeeded map. Shared by this module's and `image_retention_reconcile`'s
/// `reconcile_after_deploy`, which both run after the same shared deployment primitive.
pub(crate) fn fully_deployed_services<'a>(
    selected: &'a [ServiceEndpointPlan],
    results: &[Vec<(String, EndpointOutcome)>],
) -> BTreeMap<&'a str, bool> {
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
}

/// Reconciles cron specs for every service in `results` whose every selected endpoint deployed
/// successfully -- the shared post-processing step `jiji deploy`, `jiji service restart`, and
/// `jiji service rollback` all call after their own endpoint deployment completes (they share the
/// same `deploy_service_endpoints` primitive, but each computes `selected`/`results` itself, so
/// this takes them as parameters rather than assuming a single caller's exact variable shape).
pub(crate) async fn reconcile_after_deploy(
    ssh: &Ssh,
    config: &Config,
    plan: &NetworkPlan,
    sessions: &BTreeMap<String, Arc<SshSession>>,
    selected: &[ServiceEndpointPlan],
    results: &[Vec<(String, EndpointOutcome)>],
) -> Vec<String> {
    let mut problems = Vec::new();
    for (service_name, succeeded) in fully_deployed_services(selected, results) {
        if !succeeded {
            continue;
        }
        let service = &config.services[service_name];
        problems.extend(
            reconcile_service_crons(ssh, config, plan, service_name, service, sessions).await,
        );
    }
    problems
}

/// `jiji service remove`'s cron reconciliation: unconditional removal from every eligible server,
/// no ownership computation needed (the plan's "`jiji service remove` removes all cron
/// specifications for the selected service"). Removes every spec actually installed for the
/// service, not just names still present in `service.crons` -- a cron renamed or deleted from
/// config just before `remove` would otherwise never be cleaned up, the same gap
/// `reconcile_service_crons` closes for deploy/restart/rollback/scale. Does not stop an active run
/// (out of scope for this release, per the plan).
pub(crate) async fn remove_all_cron_specs(
    ssh: &Ssh,
    config: &Config,
    service_name: &str,
    service: &Service,
    sessions: &BTreeMap<String, Arc<SshSession>>,
) -> Vec<String> {
    let mut problems = Vec::new();
    let (sessions, newly_opened) = match resolve_sessions(
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
                    "service '{service_name}': could not reach every eligible server to remove its cron jobs: {error}"
                ));
            return problems;
        }
    };
    let none_desired = BTreeSet::new();
    for (server_name, session) in &sessions {
        problems.extend(
            remove_specs_absent_from(
                session,
                &config.project,
                service_name,
                server_name,
                &none_desired,
            )
            .await,
        );
    }
    close_newly_opened(&sessions, &newly_opened).await;
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(replica_id: &str, state: DeploymentState, health: HealthState) -> CatalogRecord {
        CatalogRecord {
            project_id: "demo".into(),
            recovery_epoch: 1,
            protocol_version: jiji_agent::catalog::CATALOG_PROTOCOL_VERSION,
            schema_version: jiji_agent::catalog::CATALOG_SCHEMA_VERSION,
            service: "twitch".into(),
            replica_id: replica_id.into(),
            owner_node_id: format!("server-for-{replica_id}"),
            owner_epoch: 1,
            revision: 3,
            deployment_id: format!("dep-{replica_id}"),
            address: "100.64.0.5".parse().unwrap(),
            ports: vec![],
            image: "ghcr.io/example/twitch-sync:latest".into(),
            state,
            health,
        }
    }

    fn assignment(replica_id: &str, ordinal: u32) -> ReplicaAssignment {
        ReplicaAssignment {
            replica_id: replica_id.into(),
            ordinal,
            server: format!("server-for-{replica_id}"),
        }
    }

    #[test]
    fn owner_is_the_lowest_ordinal_active_healthy_replica() {
        let assignments = vec![
            assignment("r0", 0),
            assignment("r1", 1),
            assignment("r2", 2),
        ];
        // r0 is Candidate (mid-deploy), r1 and r2 are Active/Healthy: r1 wins, not r0.
        let catalog = vec![
            record("r0", DeploymentState::Candidate, HealthState::Unknown),
            record("r1", DeploymentState::Active, HealthState::Healthy),
            record("r2", DeploymentState::Active, HealthState::Healthy),
        ];
        let (owner_assignment, owner_record) =
            select_cron_owner(&assignments, &catalog).expect("expected an owner");
        assert_eq!(owner_assignment.replica_id, "r1");
        assert_eq!(owner_record.replica_id, "r1");
    }

    #[test]
    fn no_active_healthy_replica_yields_no_owner() {
        let assignments = vec![assignment("r0", 0)];
        let catalog = vec![record(
            "r0",
            DeploymentState::Draining,
            HealthState::Unknown,
        )];
        assert!(select_cron_owner(&assignments, &catalog).is_none());
    }

    #[test]
    fn owner_ignores_an_unhealthy_active_replica() {
        let assignments = vec![assignment("r0", 0), assignment("r1", 1)];
        let catalog = vec![
            record("r0", DeploymentState::Active, HealthState::Unhealthy),
            record("r1", DeploymentState::Active, HealthState::Healthy),
        ];
        let (owner_assignment, _) =
            select_cron_owner(&assignments, &catalog).expect("expected an owner");
        assert_eq!(owner_assignment.replica_id, "r1");
    }

    #[test]
    fn former_owner_keeps_only_jobs_that_failed_to_install_on_the_new_owner() {
        let desired = BTreeSet::from(["cleanup", "sync"]);
        let installed = BTreeSet::from(["sync".to_string()]);

        assert_eq!(
            desired_on_former_owner(&desired, &installed),
            BTreeSet::from(["cleanup"])
        );
    }

    #[test]
    fn former_owner_keeps_no_jobs_after_all_install_on_the_new_owner() {
        let desired = BTreeSet::from(["cleanup", "sync"]);
        let installed = BTreeSet::from(["cleanup".to_string(), "sync".to_string()]);

        assert!(desired_on_former_owner(&desired, &installed).is_empty());
    }

    fn cron_config() -> CronConfig {
        serde_yaml::from_str(
            r#"
schedule: "7 */2 * * *"
command: ["npm", "run", "sync:twitch"]
timezone: America/Denver
timeout: 30m
"#,
        )
        .unwrap()
    }

    #[test]
    fn render_apply_request_carries_every_field_and_a_stable_hash() {
        let cron = cron_config();
        let request = render_apply_request(
            "twitch",
            "sync-twitch",
            &cron,
            "ghcr.io/example/twitch-sync:latest",
            &["-v".to_string(), "twitch-data:/data".to_string()],
            &["--memory".to_string(), "512m".to_string()],
            ".jiji/demo/env/twitch-app-1.env",
            "dep-a",
            "replica-a",
            "jiji-demo",
            "100.64.0.5".parse().unwrap(),
            3,
        );
        let RequestBody::CronSpecApply {
            service,
            cron_name,
            revision,
            canonical_hash,
            schedule,
            timezone,
            timeout_seconds,
            command,
            ..
        } = &request
        else {
            panic!("expected CronSpecApply");
        };
        assert_eq!(service, "twitch");
        assert_eq!(cron_name, "sync-twitch");
        assert_eq!(*revision, 3);
        assert!(!canonical_hash.is_empty());
        assert_eq!(schedule, "7 */2 * * *");
        assert_eq!(timezone, "America/Denver");
        assert_eq!(*timeout_seconds, 30 * 60);
        assert_eq!(
            command,
            &vec![
                "npm".to_string(),
                "run".to_string(),
                "sync:twitch".to_string()
            ]
        );
    }

    #[test]
    fn render_apply_request_hash_is_stable_and_ignores_revision() {
        let cron = cron_config();
        let build = |revision: u64| {
            let RequestBody::CronSpecApply { canonical_hash, .. } = render_apply_request(
                "twitch",
                "sync-twitch",
                &cron,
                "ghcr.io/example/twitch-sync:latest",
                &[],
                &[],
                ".jiji/demo/env/twitch-app-1.env",
                "dep-a",
                "replica-a",
                "jiji-demo",
                "100.64.0.5".parse().unwrap(),
                revision,
            ) else {
                panic!("expected CronSpecApply");
            };
            canonical_hash
        };
        // A revision bump alone (e.g. a redeploy with no config change) must not look like drift.
        assert_eq!(build(1), build(2));
    }

    #[test]
    fn render_apply_request_hash_changes_with_schedule() {
        let mut changed = cron_config();
        changed.schedule = "0 3 * * *".to_string();
        let RequestBody::CronSpecApply {
            canonical_hash: a, ..
        } = render_apply_request(
            "twitch",
            "sync-twitch",
            &cron_config(),
            "img",
            &[],
            &[],
            "env",
            "dep",
            "replica",
            "bridge",
            "100.64.0.5".parse().unwrap(),
            1,
        )
        else {
            panic!()
        };
        let RequestBody::CronSpecApply {
            canonical_hash: b, ..
        } = render_apply_request(
            "twitch",
            "sync-twitch",
            &changed,
            "img",
            &[],
            &[],
            "env",
            "dep",
            "replica",
            "bridge",
            "100.64.0.5".parse().unwrap(),
            1,
        )
        else {
            panic!()
        };
        assert_ne!(a, b);
    }

    #[test]
    fn owner_env_file_path_matches_env_resolutions_own_formula() {
        assert_eq!(
            owner_env_file_path("demo", "twitch", "app-1"),
            ".jiji/demo/env/twitch-app-1.env"
        );
    }

    #[test]
    fn absolutize_prefixes_the_home_directory() {
        assert_eq!(
            absolutize("/root", ".jiji/demo/env/twitch-app-1.env"),
            "/root/.jiji/demo/env/twitch-app-1.env"
        );
    }

    #[test]
    fn absolutize_mount_args_rewrites_only_jiji_relative_bind_sources() {
        let args = vec![
            "-v".to_string(),
            ".jiji/demo/files/twitch/config.yml:/app/config.yml".to_string(),
            "-v".to_string(),
            "twitch-data:/data".to_string(),
            "-v".to_string(),
            "/host/absolute:/mnt".to_string(),
        ];
        assert_eq!(
            absolutize_mount_args("/root", args),
            vec![
                "-v".to_string(),
                "/root/.jiji/demo/files/twitch/config.yml:/app/config.yml".to_string(),
                "-v".to_string(),
                "twitch-data:/data".to_string(),
                "-v".to_string(),
                "/host/absolute:/mnt".to_string(),
            ]
        );
    }
}
