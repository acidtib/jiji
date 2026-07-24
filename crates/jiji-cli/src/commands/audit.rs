use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use jiji_config::{load_config, validate_config};
use jiji_network::NetworkPlanner;
use jiji_ssh::{SshPool, SshSession};
use jiji_tui::Ui;

use crate::audit::{self, AuditEntry, AuditStatus};
use crate::commands::deploy::split_comma_trimmed;
use crate::commands::proxy::logs::stream_logs;
use crate::ssh_adapter;

#[allow(clippy::too_many_arguments)]
pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    hosts: Option<&str>,
    services: Option<&str>,
    lines: u32,
    grep: Option<&str>,
    status: Option<&str>,
    json: bool,
    follow: bool,
) -> anyhow::Result<()> {
    if services.is_some() {
        anyhow::bail!(
            "`jiji audit` does not accept -S/--services: the audit trail is per server, not per service. Use -H/--hosts to select servers instead."
        );
    }
    let status_filter = status.map(str::parse::<AuditStatus>).transpose()?;

    let start = std::env::current_dir()?;
    let (config, path) = load_config(environment, config_file.map(Path::new), &start)?;
    let validation = validate_config(&config);
    if !validation.valid {
        for error in validation.errors {
            Ui::error(&format!("{}: {}", error.path, error.message));
        }
        anyhow::bail!("Configuration is invalid; fix the errors above and try again");
    }
    let ssh = config.ssh.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "No `ssh:` section configured in {}. Add at least `ssh.user:` before running jiji audit.",
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
    if follow && selected.len() != 1 {
        anyhow::bail!(
            "-H/--hosts matched {} server(s) ({}). `jiji audit --follow` requires exactly one; narrow the filter and try again.",
            selected.len(),
            selected
                .iter()
                .map(|server| server.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
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
                sessions.insert(name.clone(), Arc::new(session));
            }
            Err(error) => failures.push(format!("{name}: {error}")),
        }
    }
    if !failures.is_empty() {
        close_all(&sessions).await;
        anyhow::bail!(
            "Could not connect to server(s): {}. Restore SSH access and retry.",
            failures.join(", ")
        );
    }

    if follow {
        let (name, session) = sessions.iter().next().expect("exactly one, checked above");
        if !json {
            Ui::section(&format!("Following audit trail on {name}:"));
        }
        let command = audit::render_follow_command(&plan.project);
        let result = stream_logs(session, &command).await;
        close_all(&sessions).await;
        return result;
    }

    let names: Vec<String> = sessions.keys().cloned().collect();
    let operations: Vec<_> = names
        .iter()
        .map(|name| sessions.get(name).expect("connected above").clone())
        .map(|session| {
            let project = plan.project.clone();
            move || async move { audit::read_entries(&session, &project, lines).await }
        })
        .collect();
    let results = pool.execute_concurrent(operations).await;
    close_all(&sessions).await;

    let mut per_host: Vec<(String, Vec<AuditEntry>)> = Vec::new();
    let mut read_failures = Vec::new();
    for (name, result) in names.into_iter().zip(results) {
        match result {
            Ok(entries) => {
                let filtered: Vec<AuditEntry> = entries
                    .into_iter()
                    .filter(|entry| status_filter.is_none_or(|status| entry.status == status))
                    .filter(|entry| {
                        grep.is_none_or(|pattern| {
                            entry.action.contains(pattern) || entry.message.contains(pattern)
                        })
                    })
                    .collect();
                per_host.push((name, filtered));
            }
            Err(error) => read_failures.push(format!("{name}: {error}")),
        }
    }
    if !read_failures.is_empty() {
        anyhow::bail!(
            "Could not read the audit trail on server(s): {}.",
            read_failures.join(", ")
        );
    }

    if json {
        for (host, entries) in &per_host {
            for entry in entries {
                let payload = serde_json::json!({
                    "host": host,
                    "timestamp": entry.timestamp,
                    "action": entry.action,
                    "status": entry.status,
                    "actor": entry.actor,
                    "message": entry.message,
                });
                println!("{}", serde_json::to_string(&payload)?);
            }
        }
        return Ok(());
    }

    Ui::section("Audit Trail:");
    let total: usize = per_host.iter().map(|(_, entries)| entries.len()).sum();
    if total == 0 {
        Ui::say("No matching audit entries.", 1);
        return Ok(());
    }
    for (host, entries) in &per_host {
        if entries.is_empty() {
            continue;
        }
        Ui::say(&format!("{host}:"), 1);
        for entry in entries {
            Ui::say(
                &format!(
                    "[{}] {} ({}) {}: {}",
                    entry.status,
                    entry.timestamp,
                    audit::format_timestamp(entry.timestamp),
                    entry.action,
                    entry.message
                ),
                2,
            );
        }
    }
    Ok(())
}

async fn close_all(sessions: &BTreeMap<String, Arc<SshSession>>) {
    for session in sessions.values() {
        session.close().await;
    }
}
