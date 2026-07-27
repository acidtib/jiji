pub mod acquire;
pub mod release;
pub mod show;
pub mod status;

use std::collections::BTreeMap;
use std::future::Future;
use std::path::Path;
use std::sync::Arc;

use jiji_config::validate_config;
use jiji_network::NetworkPlanner;
use jiji_ssh::{SshPool, SshSession};
use jiji_tui::Ui;

use crate::commands::deploy::split_comma_trimmed;
use crate::lock::LockInfo;
use crate::ssh_adapter;

/// SSH sessions for every server a `jiji lock` subcommand targets, plus the project name the
/// lock file is scoped under. Locks are host-scoped (see `connect_targets`'s `-S` rejection), so
/// unlike `deploy`/`service remove` there is no per-endpoint filtering here.
pub(crate) struct LockTargets {
    pub project: String,
    pub pool: SshPool,
    pub sessions: BTreeMap<String, Arc<SshSession>>,
}

pub(crate) async fn connect_targets(
    environment: Option<&str>,
    config_file: Option<&str>,
    hosts: Option<&str>,
    services: Option<&str>,
    quiet: bool,
) -> anyhow::Result<LockTargets> {
    if services.is_some() {
        anyhow::bail!(
            "`jiji lock` does not accept -S/--services: deployment locks are held per server, not per service. Use -H/--hosts to select servers instead."
        );
    }

    let start = std::env::current_dir()?;
    let (config, path) =
        crate::config_loading::load_config_for_ssh(environment, config_file.map(Path::new), &start)
            .await?;
    let validation = validate_config(&config);
    if !validation.valid {
        for error in validation.errors {
            Ui::error(&format!("{}: {}", error.path, error.message));
        }
        anyhow::bail!("Configuration is invalid; fix the errors above and try again");
    }
    let ssh = config.ssh.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "No `ssh:` section configured in {}. Add at least `ssh.user:` before running jiji lock.",
            path.display()
        )
    })?;
    let plan = NetworkPlanner::new()
        .plan(&config)
        .map_err(|error| anyhow::anyhow!("Could not build the private network plan: {error}"))?;

    let filters = split_comma_trimmed(hosts);
    let selected = plan.select_hosts(&filters)?;
    if selected.is_empty() {
        anyhow::bail!("No servers are configured. Add a `servers:` entry and retry.");
    }

    let mut connect_options = BTreeMap::new();
    for server_plan in &selected {
        let name = server_plan.name.clone();
        let named_server = config.servers.get(&name).cloned().ok_or_else(|| {
            anyhow::anyhow!("Server '{name}' selected by the network plan is not configured")
        })?;
        connect_options.insert(
            name.clone(),
            ssh_adapter::connect_options(&name, &named_server, &ssh)?,
        );
    }

    let pool = SshPool::new(ssh.max_concurrent_starts as usize);
    let names: Vec<String> = connect_options.keys().cloned().collect();
    let operations: Vec<_> = names
        .iter()
        .map(|name| connect_options.get(name).expect("inserted above").clone())
        .map(|options| move || async move { SshSession::connect(&options).await })
        .collect();
    let connections = pool.execute_concurrent(operations).await;

    let mut sessions: BTreeMap<String, Arc<SshSession>> = BTreeMap::new();
    let mut failures = Vec::new();
    for (name, connection) in names.iter().zip(connections) {
        match connection {
            Ok(session) => {
                if !quiet {
                    Ui::say(&format!("{name}: connected"), 1);
                }
                sessions.insert(name.clone(), Arc::new(session));
            }
            Err(error) => {
                if !quiet {
                    Ui::error(&format!("{name}: {error}"));
                }
                failures.push(format!("{name}: {error}"));
            }
        }
    }
    if !failures.is_empty() {
        close_all(&sessions).await;
        anyhow::bail!(
            "Could not connect to server(s): {}. Restore SSH access and retry.",
            failures.join(", ")
        );
    }

    Ok(LockTargets {
        project: config.project,
        pool,
        sessions,
    })
}

/// Reads the lock, if any, on every target host concurrently. Results are returned in the same
/// (sorted, since `sessions` is a `BTreeMap`) host-name order every time, so callers can render a
/// stable listing.
pub(super) async fn read_all(
    targets: &LockTargets,
) -> anyhow::Result<Vec<(String, Option<LockInfo>)>> {
    let names: Vec<String> = targets.sessions.keys().cloned().collect();
    let operations: Vec<_> = names
        .iter()
        .map(|name| targets.sessions.get(name).expect("connected above").clone())
        .map(|session| {
            let project = targets.project.clone();
            move || async move { crate::lock::read_lock(&session, &project).await }
        })
        .collect();
    let results = targets.pool.execute_concurrent(operations).await;

    names
        .into_iter()
        .zip(results)
        .map(|(name, result)| result.map(|info| (name, info)))
        .collect()
}

pub(crate) async fn close_all(sessions: &BTreeMap<String, Arc<SshSession>>) {
    for session in sessions.values() {
        session.close().await;
    }
}

#[derive(Clone, Copy)]
pub(crate) struct AutomaticLockOptions {
    pub timeout: u64,
    pub force: bool,
}

pub(crate) async fn with_deployment_lock<F, Fut>(
    environment: Option<&str>,
    config_file: Option<&str>,
    hosts: Option<&str>,
    project: &str,
    message: String,
    options: AutomaticLockOptions,
    operation: F,
) -> anyhow::Result<()>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    Ui::section("Acquiring Deployment Lock:");
    let targets = connect_targets(environment, config_file, hosts, None, true).await?;
    let locks = match crate::lock::OwnedDeploymentLocks::acquire(
        &targets.pool,
        &targets.sessions,
        project,
        message,
        options.timeout,
        options.force,
    )
    .await
    {
        Ok(locks) => locks,
        Err(error) => {
            close_all(&targets.sessions).await;
            return Err(error);
        }
    };
    Ui::say(
        &format!("Acquired on {} server(s).", targets.sessions.len()),
        1,
    );

    let operation_result = operation().await;
    Ui::section("Releasing Deployment Lock:");
    let release_result = locks.release(&targets.pool, &targets.sessions).await;
    close_all(&targets.sessions).await;
    match (operation_result, release_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(release_error)) => Err(release_error),
        (Err(error), Err(release_error)) => {
            Err(error.context(format!("Additionally, {release_error}")))
        }
    }
}
