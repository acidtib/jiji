use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use jiji_config::validate_config;
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
    stats: bool,
    since: Option<&str>,
    follow: bool,
) -> anyhow::Result<()> {
    if services.is_some() {
        anyhow::bail!(
            "`jiji audit` does not accept -S/--services: the audit trail is per server, not per service. Use -H/--hosts to select servers instead."
        );
    }
    if stats && follow {
        anyhow::bail!(
            "`jiji audit --stats` cannot be combined with --follow. Remove one of those flags and retry."
        );
    }
    let since_seconds = since.map(parse_window).transpose()?;
    let cutoff = since_seconds.map(|window| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0)
            .saturating_sub(window)
    });
    let status_filter = status.map(str::parse::<AuditStatus>).transpose()?;

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

    let connecting_started = std::time::Instant::now();
    let pool = SshPool::new(ssh.max_concurrent_starts as usize);
    let names: Vec<String> = connect_options.keys().cloned().collect();
    let audit_progress = if !json {
        let p = jiji_tui::ServerSetupProgress::with_title(names.clone(), "Connecting".to_string());
        let h = p.handle();
        for n in &names {
            h.set_status(n, "connecting");
        }
        Ui::section("Connecting:");
        Some((p, h))
    } else {
        None
    };
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
                if let Some((_, h)) = &audit_progress {
                    h.mark_success(name, "connected");
                }
                if !json {
                    Ui::say(&format!("{name}: connected"), 1);
                }
                sessions.insert(name.clone(), Arc::new(session));
            }
            Err(error) => {
                if let Some((_, h)) = &audit_progress {
                    h.mark_failed(name, &error.to_string());
                }
                failures.push(format!("{name}: {error}"))
            }
        }
    }
    if let Some((p, _)) = audit_progress {
        p.finish();
    }
    if !json {
        Ui::say(
            &format!(
                "Connected in {}",
                jiji_tui::format_duration(connecting_started.elapsed())
            ),
            1,
        );
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
            move || async move {
                if let Some(cutoff) = cutoff {
                    audit::read_entries_since(&session, &project, cutoff).await
                } else if stats {
                    audit::read_all_entries(&session, &project).await
                } else {
                    audit::read_entries(&session, &project, lines).await
                }
            }
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
                    .filter(|entry| cutoff.is_none_or(|cutoff| entry.timestamp >= cutoff))
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

    if stats {
        return render_stats(&per_host, json);
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
                    "duration_ms": entry.duration_ms,
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
            let duration = entry
                .duration_ms
                .map(|ms| format!(" [{}]", audit::format_duration_ms(ms)))
                .unwrap_or_default();
            Ui::say(
                &format!(
                    "[{}] {} ({}) {}{}: {}",
                    entry.status,
                    entry.timestamp,
                    audit::format_timestamp(entry.timestamp),
                    entry.action,
                    duration,
                    entry.message
                ),
                2,
            );
        }
    }
    Ok(())
}

#[derive(Debug, Clone, serde::Serialize)]
struct StatsRow {
    entries: usize,
    successes: usize,
    failures: usize,
    success_rate: f64,
    average_duration_ms: Option<u64>,
    measured_durations: usize,
}

fn summarize<'a>(entries: impl Iterator<Item = &'a AuditEntry>) -> StatsRow {
    let entries: Vec<&AuditEntry> = entries.collect();
    let successes = entries
        .iter()
        .filter(|entry| entry.status == AuditStatus::Success)
        .count();
    let durations: Vec<u64> = entries
        .iter()
        .filter_map(|entry| entry.duration_ms)
        .collect();
    StatsRow {
        entries: entries.len(),
        successes,
        failures: entries.len().saturating_sub(successes),
        success_rate: if entries.is_empty() {
            0.0
        } else {
            successes as f64 * 100.0 / entries.len() as f64
        },
        average_duration_ms: (!durations.is_empty()).then(|| {
            (durations
                .iter()
                .map(|duration| *duration as u128)
                .sum::<u128>()
                / durations.len() as u128) as u64
        }),
        measured_durations: durations.len(),
    }
}

fn render_stats(per_host: &[(String, Vec<AuditEntry>)], json: bool) -> anyhow::Result<()> {
    let overall = summarize(per_host.iter().flat_map(|(_, entries)| entries.iter()));
    let mut by_action: BTreeMap<String, Vec<&AuditEntry>> = BTreeMap::new();
    for entry in per_host.iter().flat_map(|(_, entries)| entries) {
        by_action
            .entry(entry.action.clone())
            .or_default()
            .push(entry);
    }
    let actions: BTreeMap<String, StatsRow> = by_action
        .into_iter()
        .map(|(action, entries)| (action, summarize(entries.into_iter())))
        .collect();
    let servers: BTreeMap<String, StatsRow> = per_host
        .iter()
        .map(|(host, entries)| (host.clone(), summarize(entries.iter())))
        .collect();

    if json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "overall": overall,
                "by_action": actions,
                "by_server": servers,
            }))?
        );
        return Ok(());
    }

    Ui::section("Audit Statistics:");
    if overall.entries == 0 {
        Ui::say("No matching audit entries.", 1);
        return Ok(());
    }
    Ui::say("Overall:", 1);
    render_stats_row("all", &overall, 2);
    Ui::say("By action:", 1);
    for (action, row) in &actions {
        render_stats_row(action, row, 2);
    }
    Ui::say("By server:", 1);
    for (server, row) in &servers {
        render_stats_row(server, row, 2);
    }
    Ok(())
}

fn render_stats_row(label: &str, row: &StatsRow, indent: u8) {
    let average = row
        .average_duration_ms
        .map(audit::format_duration_ms)
        .unwrap_or_else(|| "n/a".to_string());
    Ui::say(
        &format!(
            "{label}: {} entries, {} success, {} failed, {:.1}% success, avg {} ({}/{} timed)",
            row.entries,
            row.successes,
            row.failures,
            row.success_rate,
            average,
            row.measured_durations,
            row.entries
        ),
        indent,
    );
}

fn parse_window(value: &str) -> anyhow::Result<u64> {
    let value = value.trim();
    let split = value
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(value.len());
    let (amount, unit) = value.split_at(split);
    let amount = amount.parse::<u64>().map_err(|_| {
        anyhow::anyhow!(
            "Invalid --since window '{value}'. Use a positive duration such as 30m, 12h, or 7d."
        )
    })?;
    let multiplier = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 60 * 60,
        "d" => 24 * 60 * 60,
        "w" => 7 * 24 * 60 * 60,
        _ => {
            anyhow::bail!(
                "Invalid --since window '{value}'. Use a positive duration such as 30m, 12h, or 7d."
            )
        }
    };
    if amount == 0 {
        anyhow::bail!(
            "Invalid --since window '{value}'. Use a positive duration such as 30m, 12h, or 7d."
        );
    }
    amount.checked_mul(multiplier).ok_or_else(|| {
        anyhow::anyhow!(
            "The --since window '{value}' is too large. Use a smaller duration and retry."
        )
    })
}

async fn close_all(sessions: &BTreeMap<String, Arc<SshSession>>) {
    for session in sessions.values() {
        session.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_compact_relative_windows() {
        assert_eq!(parse_window("30m").unwrap(), 1800);
        assert_eq!(parse_window("12h").unwrap(), 43_200);
        assert_eq!(parse_window("7d").unwrap(), 604_800);
        assert!(parse_window("0m").is_err());
        assert!(parse_window("yesterday").is_err());
    }

    #[test]
    fn summary_uses_all_results_but_only_measured_durations() {
        let entries = [
            AuditEntry {
                timestamp: 1,
                action: "deploy".into(),
                status: AuditStatus::Success,
                actor: "a".into(),
                message: "ok".into(),
                duration_ms: Some(100),
                lock_scope: None,
                deployment_id: None,
            },
            AuditEntry {
                timestamp: 2,
                action: "deploy".into(),
                status: AuditStatus::Failed,
                actor: "a".into(),
                message: "failed".into(),
                duration_ms: None,
                lock_scope: None,
                deployment_id: None,
            },
        ];
        let row = summarize(entries.iter());
        assert_eq!(row.entries, 2);
        assert_eq!(row.successes, 1);
        assert_eq!(row.failures, 1);
        assert_eq!(row.success_rate, 50.0);
        assert_eq!(row.average_duration_ms, Some(100));
        assert_eq!(row.measured_durations, 1);
    }
}
