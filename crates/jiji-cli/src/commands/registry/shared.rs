use std::collections::BTreeMap;
use std::sync::Arc;

use jiji_config::{Config, NamedServer};
use jiji_network::NetworkPlanner;
use jiji_ssh::{SshPool, SshSession};
use jiji_tui::Ui;

use crate::ssh_adapter;

/// A single local-or-remote target that a registry auth command attempted (or skipped).
pub enum TargetKind {
    Local,
    Remote(String),
}

impl TargetKind {
    fn label(&self) -> &str {
        match self {
            TargetKind::Local => "Local",
            TargetKind::Remote(name) => name,
        }
    }
}

pub enum TargetOutcome {
    Success(TargetKind),
    AlreadyDone(TargetKind),
    Skipped(TargetKind),
    Failed(TargetKind, String),
}

pub fn split_comma_trimmed(value: Option<&str>) -> Vec<String> {
    value
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

/// Rejects the global `-S`/`--services` filter and the invalid both-skip-flags combination
/// before any credential resolution or side effect. Registry credentials belong to an engine
/// user on a host, not to an individual service.
pub fn validate_scope(
    services: Option<&str>,
    skip_local: bool,
    skip_remote: bool,
) -> anyhow::Result<()> {
    if services.is_some() {
        anyhow::bail!(
            "Registry login/logout does not accept -S/--services: credentials belong to a host's container engine, not an individual service."
        );
    }
    if skip_local && skip_remote {
        anyhow::bail!(
            "--skip-local and --skip-remote cannot be used together: there would be nothing left to do."
        );
    }
    Ok(())
}

/// Selects the configured servers targeted by `-H`/`--hosts`, using the same name/address/
/// wildcard matching as other `-H` aware commands. Does not require `network.enabled`: the
/// server list in a `NetworkPlan` is always computed from `config.servers` regardless.
pub fn select_target_servers(
    config: &Config,
    hosts: Option<&str>,
) -> anyhow::Result<Vec<(String, NamedServer)>> {
    let plan = NetworkPlanner::new()
        .plan(config)
        .map_err(|error| anyhow::anyhow!("Could not build the private network plan: {error}"))?;
    let host_filters = split_comma_trimmed(hosts);
    let mut selected: Vec<(String, NamedServer)> = plan
        .select_hosts(&host_filters)?
        .into_iter()
        .filter_map(|server| {
            config
                .servers
                .get(&server.name)
                .map(|named| (server.name.clone(), named.clone()))
        })
        .collect();
    selected.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(selected)
}

/// Connects to every selected server with bounded concurrency, tolerating per-host failures:
/// an unreachable host is recorded as a `Failed` outcome rather than aborting the whole run, so
/// the remaining hosts still get attempted.
pub async fn connect_all(
    ssh: &jiji_config::Ssh,
    servers: &[(String, NamedServer)],
) -> anyhow::Result<(BTreeMap<String, Arc<SshSession>>, Vec<TargetOutcome>)> {
    let pool = SshPool::new(ssh.max_concurrent_starts as usize);
    let mut connect_options = Vec::with_capacity(servers.len());
    for (name, server) in servers {
        connect_options.push(ssh_adapter::connect_options(name, server, ssh)?);
    }

    Ui::section("Connecting:");
    let operations: Vec<_> = connect_options
        .into_iter()
        .map(|options| move || async move { SshSession::connect(&options).await })
        .collect();
    let connections = pool.execute_concurrent(operations).await;

    let mut sessions: BTreeMap<String, Arc<SshSession>> = BTreeMap::new();
    let mut outcomes = Vec::new();
    for ((name, server), connection) in servers.iter().zip(connections) {
        match connection {
            Ok(session) => {
                Ui::say(&format!("{name} ({}): connected", server.host), 1);
                sessions.insert(name.clone(), Arc::new(session));
            }
            Err(error) => {
                Ui::error(&format!("{name} ({}): {error}", server.host));
                outcomes.push(TargetOutcome::Failed(
                    TargetKind::Remote(name.clone()),
                    error.to_string(),
                ));
            }
        }
    }
    Ok((sessions, outcomes))
}

pub async fn close_all(sessions: &BTreeMap<String, Arc<SshSession>>) {
    for session in sessions.values() {
        session.close().await;
    }
}

/// Prints the per-target summary and turns any failure into a non-zero exit, in the shape
/// described by the registry auth commands plan: one line per target, then a trailing count.
pub fn report(
    command: &str,
    outcomes: Vec<TargetOutcome>,
    done_word: &str,
    idempotent_word: &str,
) -> anyhow::Result<()> {
    let mut attempted = 0usize;
    let mut failures = 0usize;
    for outcome in &outcomes {
        match outcome {
            TargetOutcome::Success(kind) => {
                Ui::say(&format!("{}: {done_word}", kind.label()), 1);
                attempted += 1;
            }
            TargetOutcome::AlreadyDone(kind) => {
                Ui::say(&format!("{}: {idempotent_word}", kind.label()), 1);
                attempted += 1;
            }
            TargetOutcome::Skipped(kind) => {
                Ui::say(&format!("{}: skipped", kind.label()), 1);
            }
            TargetOutcome::Failed(kind, error) => {
                Ui::error(&format!("{}: {error}", kind.label()));
                attempted += 1;
                failures += 1;
            }
        }
    }
    println!();
    if failures > 0 {
        anyhow::bail!(
            "{command} failed on {failures} of {attempted} target(s); see the per-target errors above."
        );
    }
    Ui::success(&format!("{command} completed on {attempted} target(s)."));
    Ok(())
}
