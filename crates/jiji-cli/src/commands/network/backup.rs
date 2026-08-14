use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::future::Future;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;

use jiji_agent::backup::AgentBackupSnapshot;
use jiji_agent::AgentPaths;
use jiji_config::validate_config;
use jiji_network::{NetworkPlanner, ServerPlan};
use jiji_ssh::{SshPool, SshSession};
use jiji_tui::Ui;
use serde::{Deserialize, Serialize};

use crate::audit::{self, AuditStatus};
use crate::lock::{LockRequest, LockScope};
use crate::ssh_adapter;

/// A dedicated connection purely to hold the project-maintenance lock around `operation`: backup
/// export/restore each manage their own independent per-server connect/close cycle, so there is
/// no single persistent session set to reuse here (mirrors the same pattern in `server::setup`
/// and `network::setup::run`). Takes `project`/`servers` as independent values rather than a
/// borrowed `&Config`, so a caller whose `operation` closure needs to move its own `Config` is
/// never forced into borrowing and moving the same value in one expression.
pub(crate) async fn with_project_maintenance_lock<F, Fut>(
    project: &str,
    servers: &std::collections::HashMap<String, jiji_config::NamedServer>,
    ssh: &jiji_config::Ssh,
    selected: &[ServerPlan],
    message: String,
    operation: F,
) -> anyhow::Result<()>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    let pool = SshPool::new(ssh.max_concurrent_starts as usize);
    let mut lock_connect_options = Vec::with_capacity(selected.len());
    for server_plan in selected {
        let server = servers
            .get(&server_plan.name)
            .ok_or_else(|| anyhow::anyhow!("Server '{}' is not configured", server_plan.name))?;
        lock_connect_options.push(ssh_adapter::connect_options(
            &server_plan.name,
            server,
            ssh,
        )?);
    }
    let operations: Vec<_> = lock_connect_options
        .into_iter()
        .map(|options| move || async move { SshSession::connect(&options).await })
        .collect();
    let connections = pool.execute_concurrent(operations).await;
    let mut lock_sessions: BTreeMap<String, Arc<SshSession>> = BTreeMap::new();
    let mut lock_failures = Vec::new();
    for (server_plan, connection) in selected.iter().zip(connections) {
        match connection {
            Ok(session) => {
                lock_sessions.insert(server_plan.name.clone(), Arc::new(session));
            }
            Err(error) => lock_failures.push(format!("{}: {error}", server_plan.name)),
        }
    }
    if !lock_failures.is_empty() {
        for session in lock_sessions.values() {
            session.close().await;
        }
        anyhow::bail!(
            "Could not connect to server(s): {}. Restore SSH access and retry.",
            lock_failures.join(", ")
        );
    }
    let lock_requests: Vec<LockRequest> = selected
        .iter()
        .map(|server_plan| {
            LockRequest::new(LockScope::ProjectMaintenance, server_plan.name.clone())
        })
        .collect();

    let result = crate::commands::lock::with_locks(
        &pool,
        &lock_sessions,
        project,
        lock_requests,
        message,
        crate::commands::lock::AutomaticLockOptions {
            timeout: 300,
            force: false,
        },
        operation,
    )
    .await;
    for session in lock_sessions.values() {
        session.close().await;
    }
    result
}

const OPERATOR_BACKUP_FORMAT_VERSION: u16 = 2;

/// Deployment/catalog history only -- membership is never part of this backup. It's trivially
/// re-derived from `jiji.yml` and pushed fresh by `jiji server setup` (the same computation it
/// always runs), so there's no key or membership state to protect or restore here; see
/// `jiji_agent::backup`'s module doc comment.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct OperatorBackup {
    format_version: u16,
    project_id: String,
    recovery_epoch: u64,
    agents: Vec<AgentBackupSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RecoverySource {
    format_version: u16,
    project_id: String,
    source_recovery_epoch: u64,
    replacement_recovery_epoch: u64,
    agents: Vec<AgentBackupSnapshot>,
}

pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    hosts: Option<&str>,
    output: &Path,
    passphrase_file: &Path,
) -> anyhow::Result<()> {
    let start = std::env::current_dir()?;
    let (config, path) =
        crate::config_loading::load_config_for_ssh(environment, config_file.map(Path::new), &start)
            .await?;
    if !validate_config(&config).valid {
        anyhow::bail!("Configuration is invalid; fix it before exporting a backup");
    }
    if output.exists() {
        anyhow::bail!(
            "{} already exists; choose a new backup path so an older recovery point is not overwritten",
            output.display()
        );
    }
    let passphrase = read_private_passphrase(passphrase_file)?;
    let recovery_epoch = crate::recovery_epoch::read(&path)?;
    let ssh = config
        .ssh
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No `ssh:` section is configured"))?;
    let plan = NetworkPlanner::new().plan(&config)?;
    let filters = split_comma_trimmed(hosts);
    let selected: Vec<ServerPlan> = plan.select_hosts(&filters)?.into_iter().cloned().collect();
    if selected.is_empty() {
        anyhow::bail!("No servers are configured; there is no distributed state to export");
    }

    let selected_for_lock = selected.clone();
    let project_for_lock = config.project.clone();
    let servers_for_lock = config.servers.clone();
    with_project_maintenance_lock(
        &project_for_lock,
        &servers_for_lock,
        ssh,
        &selected_for_lock,
        "jiji network backup".to_string(),
        move || async move {
            Ui::section("Exporting Control Plane:");
            let export_started = std::time::Instant::now();
            let hosts: Vec<String> = selected.iter().map(|s| s.name.clone()).collect();
            let progress =
                jiji_tui::ServerSetupProgress::with_title(hosts.clone(), "Exporting".to_string());
            let handle = progress.handle();
            let paths = AgentPaths::default_for_project(&config.project);
            let mut snapshots = Vec::new();
            let mut failures = Vec::new();
            for server_plan in &selected {
                handle.set_status(&server_plan.name, "exporting");
                let name = server_plan.name.clone();
                let server = config
                    .servers
                    .get(&name)
                    .ok_or_else(|| anyhow::anyhow!("Server '{name}' is not configured"))?;
                let options = ssh_adapter::connect_options(&name, server, ssh)?;
                match fetch_snapshot(&options, &paths, &config.project).await {
                    Ok(snapshot) => {
                        snapshot.validate_identity(&config.project, recovery_epoch)?;
                        handle.mark_success(&name, "exported");
                        Ui::result_ok(&name, "catalog/desired state and local claims exported");
                        snapshots.push(snapshot);
                    }
                    Err(error) => {
                        handle.mark_failed(&name, &error.to_string());
                        Ui::result_warn(&name, &format!("unavailable: {error}"));
                        failures.push(name);
                    }
                }
            }
            progress.finish();
            Ui::say(
                &format!(
                    "Exported from {} host(s) in {}",
                    hosts.len(),
                    jiji_tui::format_duration(export_started.elapsed())
                ),
                1,
            );
            if snapshots.is_empty() {
                anyhow::bail!("No agent could export state; no backup was written");
            }
            let backup = OperatorBackup {
                format_version: OPERATOR_BACKUP_FORMAT_VERSION,
                project_id: config.project.clone(),
                recovery_epoch,
                agents: snapshots,
            };
            validate_backup(&backup, &config.project)?;
            let plaintext = serde_json::to_vec(&backup)?;
            let encrypted = crate::backup_crypto::encrypt(&plaintext, &passphrase)?;
            write_new_private_file(output, &encrypted)?;
            if !failures.is_empty() {
                Ui::warn(&format!(
                    "Backup is recoverable from replicated state, but local address claims from {} \
                     unavailable host(s) are absent: {}",
                    failures.len(),
                    failures.join(", ")
                ));
            }
            Ui::success_elapsed(
                &format!(
                    "Encrypted control-plane backup written to {}.",
                    output.display()
                ),
                export_started.elapsed(),
            );
            Ok(())
        },
    )
    .await
}

pub async fn recover(
    environment: Option<&str>,
    config_file: Option<&str>,
    input: &Path,
    passphrase_file: &Path,
    yes: bool,
) -> anyhow::Result<()> {
    let start = std::env::current_dir()?;
    let (config, path) = jiji_config::load_config(environment, config_file.map(Path::new), &start)?;
    if !validate_config(&config).valid {
        anyhow::bail!("Configuration is invalid; fix it before recovering the control plane");
    }
    let passphrase = read_private_passphrase(passphrase_file)?;
    let encrypted = fs::read(input)?;
    let plaintext = crate::backup_crypto::decrypt(&encrypted, &passphrase)?;
    let backup: OperatorBackup = serde_json::from_slice(&plaintext)?;
    validate_backup(&backup, &config.project)?;
    if backup.project_id != config.project {
        anyhow::bail!(
            "backup belongs to project '{}', not configured project '{}'",
            backup.project_id,
            config.project
        );
    }
    let current_epoch = crate::recovery_epoch::read(&path)?;
    if current_epoch > backup.recovery_epoch {
        anyhow::bail!(
            "configured recovery epoch {current_epoch} is newer than backup epoch {}; refusing stale recovery",
            backup.recovery_epoch
        );
    }
    let next_epoch = current_epoch.max(backup.recovery_epoch).saturating_add(1);
    if !yes
        && !Ui::confirm_typed(
            &format!(
                "This fences every epoch-{current_epoch} node and prepares replacement recovery. \
                 Type the project name to continue"
            ),
            &config.project,
        )?
    {
        anyhow::bail!("Control-plane recovery cancelled.");
    }
    crate::recovery_epoch::write(&path, next_epoch)?;
    let recovery_state = crate::recovery_epoch::directory(&path)?.join(format!(
        "recovery-source-epoch-{}.json",
        backup.recovery_epoch
    ));
    let public_recovery = RecoverySource {
        format_version: backup.format_version,
        project_id: backup.project_id,
        source_recovery_epoch: backup.recovery_epoch,
        replacement_recovery_epoch: next_epoch,
        agents: backup.agents,
    };
    write_new_private_file(
        &recovery_state,
        &serde_json::to_vec_pretty(&public_recovery)?,
    )?;
    Ui::success(&format!(
        "Recovery epoch {next_epoch} prepared. Old nodes are fenced; run `jiji server setup` on \
         replacement hosts (this re-derives and pushes fresh membership for the new epoch), then \
         redeploy desired services. Historical state is at {}.",
        recovery_state.display()
    ));
    Ok(())
}

/// After replacement agents have joined the advanced epoch, re-commit the latest desired placement
/// from the recovery archive through one fresh agent. Old catalog deployments remain historical
/// and fenced; this restores scale intent without pretending old containers are active.
pub async fn replay_recovery_desired_state(
    config_path: &Path,
    config: &jiji_config::Config,
    servers: &[(String, jiji_config::NamedServer)],
    ssh: &jiji_config::Ssh,
) -> anyhow::Result<usize> {
    let current_epoch = crate::recovery_epoch::read(config_path)?;
    if current_epoch <= 1 {
        return Ok(0);
    }
    let source_path = crate::recovery_epoch::directory(config_path)?
        .join(format!("recovery-source-epoch-{}.json", current_epoch - 1));
    if !source_path.exists() {
        return Ok(0);
    }
    let source: RecoverySource = serde_json::from_slice(&fs::read(&source_path)?)?;
    if source.project_id != config.project || source.replacement_recovery_epoch != current_epoch {
        anyhow::bail!(
            "{} does not match the active recovery epoch",
            source_path.display()
        );
    }
    let mut desired =
        std::collections::BTreeMap::<String, jiji_agent::desired::DesiredStateRecord>::new();
    for snapshot in &source.agents {
        for record in &snapshot.desired {
            let replace = desired
                .get(&record.service)
                .is_none_or(|current| record.revision > current.revision);
            if replace {
                desired.insert(record.service.clone(), record.clone());
            }
        }
    }
    if desired.is_empty() || servers.is_empty() {
        return Ok(0);
    }
    let (seed_name, seed_server) = &servers[0];
    let options = ssh_adapter::connect_options(seed_name, seed_server, ssh)?;
    let session = SshSession::connect(&options).await?;
    let result = async {
        for record in desired.values() {
            crate::agent_client::call(
                &session,
                &config.project,
                Some(format!(
                    "recovery-desired:{}:{current_epoch}",
                    record.service
                )),
                jiji_agent::api::RequestBody::DesiredCommit {
                    service: record.service.clone(),
                    replica_override: record.replica_override,
                    assignments: record.assignments.clone(),
                },
            )
            .await?;
        }
        anyhow::Ok(desired.len())
    }
    .await;
    session.close().await;
    let replayed = result?;
    let replayed_path = source_path.with_extension(format!("replayed-epoch-{current_epoch}.json"));
    fs::rename(source_path, replayed_path)?;
    Ok(replayed)
}

pub async fn restore(
    environment: Option<&str>,
    config_file: Option<&str>,
    hosts: Option<&str>,
    input: &Path,
    passphrase_file: &Path,
) -> anyhow::Result<()> {
    let start = std::env::current_dir()?;
    let (config, path) =
        crate::config_loading::load_config_for_ssh(environment, config_file.map(Path::new), &start)
            .await?;
    if !validate_config(&config).valid {
        anyhow::bail!("Configuration is invalid; fix it before restoring the control plane");
    }
    let passphrase = read_private_passphrase(passphrase_file)?;
    let plaintext = crate::backup_crypto::decrypt(&fs::read(input)?, &passphrase)?;
    let backup: OperatorBackup = serde_json::from_slice(&plaintext)?;
    validate_backup(&backup, &config.project)?;
    let current_epoch = crate::recovery_epoch::read(&path)?;
    if backup.recovery_epoch != current_epoch {
        anyhow::bail!(
            "backup epoch {} does not match configured epoch {current_epoch}; use `jiji network \
             recover` after total cluster loss instead of merging epochs",
            backup.recovery_epoch
        );
    }
    let ssh = config
        .ssh
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No `ssh:` section is configured"))?;
    let plan = NetworkPlanner::new().plan(&config)?;
    let selected: Vec<ServerPlan> = plan
        .select_hosts(&split_comma_trimmed(hosts))?
        .into_iter()
        .cloned()
        .collect();

    let selected_for_lock = selected.clone();
    let project_for_lock = config.project.clone();
    let servers_for_lock = config.servers.clone();
    with_project_maintenance_lock(
        &project_for_lock,
        &servers_for_lock,
        ssh,
        &selected_for_lock,
        "jiji network restore".to_string(),
        move || async move {
            let paths = AgentPaths::default_for_project(&config.project);
            Ui::section("Restoring Control Plane:");
            let restore_started = std::time::Instant::now();
            let hosts: Vec<String> = selected.iter().map(|s| s.name.clone()).collect();
            let progress =
                jiji_tui::ServerSetupProgress::with_title(hosts.clone(), "Restoring".to_string());
            let handle = progress.handle();
            let mut failures = Vec::new();
            for server_plan in &selected {
                let host_started = std::time::Instant::now();
                handle.set_status(&server_plan.name, "restoring");
                let name = &server_plan.name;
                let exact_snapshot = backup
                    .agents
                    .iter()
                    .find(|snapshot| snapshot.node_id == *name);
                let mut snapshot = exact_snapshot
                    .or_else(|| backup.agents.first())
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("backup contains no agent snapshots"))?;
                if exact_snapshot.is_none() {
                    // Catalog/desired winners are safe to seed anywhere. Host-local claims are
                    // not: never transplant another node's leases just because that node
                    // supplied the only reachable backup snapshot.
                    snapshot.address_leases.clear();
                }
                let options = ssh_adapter::connect_options(name, &config.servers[name], ssh)?;
                let session = match SshSession::connect(&options).await {
                    Ok(session) => session,
                    Err(error) => {
                        // No session was ever opened for this host, so there is nothing to
                        // write a per-server audit entry through.
                        handle.mark_failed(name, &error.to_string());
                        failures.push(format!("{name}: {error}"));
                        continue;
                    }
                };
                let remote_input = paths.state_dir.join("restore-input.json");
                let command = format!(
                    "install -m 0600 /dev/stdin {input}; \
             {binary} backup-import --project {project} --state-dir {state} \
             --mesh-config {mesh} --input {input}; code=$?; rm -f {input}; exit $code",
                    input = remote_input.display(),
                    binary = paths.binary_path.display(),
                    project = config.project,
                    state = paths.state_dir.display(),
                    mesh = paths.mesh_config_path.display(),
                );
                let result = session
                    .execute_with_input(&command, &serde_json::to_vec(&snapshot)?)
                    .await;
                let (audit_status, summary) = match &result {
                    Ok(result) if result.success => (
                        AuditStatus::Success,
                        "same-epoch state restored".to_string(),
                    ),
                    Ok(result) => (AuditStatus::Failed, result.stderr.trim().to_string()),
                    Err(error) => (AuditStatus::Failed, error.to_string()),
                };
                audit::record(
                    &session,
                    &config.project,
                    "network_restore",
                    audit_status,
                    summary.clone(),
                    Some(&LockScope::ProjectMaintenance.to_string()),
                    None,
                    Some(host_started.elapsed()),
                )
                .await;
                session.close().await;
                match audit_status {
                    AuditStatus::Success => {
                        handle.mark_success(name, "restored");
                        Ui::result_ok(name, &summary);
                    }
                    AuditStatus::Failed => {
                        handle.mark_failed(name, &summary);
                        failures.push(format!("{name}: {summary}"));
                    }
                }
            }
            progress.finish();
            Ui::say(
                &format!(
                    "Restored to {} host(s) in {}",
                    hosts.len(),
                    jiji_tui::format_duration(restore_started.elapsed())
                ),
                1,
            );
            for failure in &failures {
                Ui::result_warn("restore", failure);
            }
            if !failures.is_empty() {
                anyhow::bail!("Restore failed on {} server(s)", failures.len());
            }
            Ui::success_elapsed("Control-plane state restored.", restore_started.elapsed());
            Ok(())
        },
    )
    .await
}

async fn fetch_snapshot(
    options: &jiji_ssh::ConnectOptions,
    paths: &AgentPaths,
    project: &str,
) -> anyhow::Result<AgentBackupSnapshot> {
    let session = SshSession::connect(options).await?;
    let result = session
        .execute(&format!(
            "{} backup-export --project {} --state-dir {} --mesh-config {}",
            paths.binary_path.display(),
            project,
            paths.state_dir.display(),
            paths.mesh_config_path.display(),
        ))
        .await;
    session.close().await;
    let result = result?;
    if !result.success {
        anyhow::bail!("{}", result.stderr.trim());
    }
    Ok(serde_json::from_str(&result.stdout)?)
}

fn validate_backup(backup: &OperatorBackup, expected_project: &str) -> anyhow::Result<()> {
    if backup.format_version != OPERATOR_BACKUP_FORMAT_VERSION {
        anyhow::bail!("unsupported control-plane backup format");
    }
    if backup.project_id != expected_project || backup.recovery_epoch == 0 {
        anyhow::bail!("control-plane backup identity is invalid");
    }
    for snapshot in &backup.agents {
        snapshot.validate_identity(&backup.project_id, backup.recovery_epoch)?;
    }
    Ok(())
}

fn read_private_passphrase(path: &Path) -> anyhow::Result<Vec<u8>> {
    let metadata = fs::metadata(path)?;
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        anyhow::bail!(
            "{} permissions are {:o}; backup passphrase files must be mode 0600 or stricter",
            path.display(),
            mode
        );
    }
    let mut value = fs::read(path)?;
    while value
        .last()
        .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
    {
        value.pop();
    }
    if value.is_empty() {
        anyhow::bail!("backup passphrase file is empty");
    }
    Ok(value)
}

fn write_new_private_file(path: &Path, content: &[u8]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(content)?;
    file.sync_all()?;
    Ok(())
}

fn split_comma_trimmed(value: Option<&str>) -> Vec<String> {
    value
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn passphrase_file_must_be_private_and_nonempty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("passphrase");
        fs::write(&path, "secret\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_private_passphrase(&path).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read_private_passphrase(&path).unwrap(), b"secret");
    }

    #[test]
    fn backup_output_refuses_to_overwrite_recovery_points() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("backup");
        write_new_private_file(&path, b"one").unwrap();
        assert!(write_new_private_file(&path, b"two").is_err());
        assert_eq!(fs::read(path).unwrap(), b"one");
    }
}
