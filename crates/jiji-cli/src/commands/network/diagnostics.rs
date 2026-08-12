use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use jiji_agent::api::{RequestBody, ResponseBody};
use jiji_config::validate_config;
use jiji_network::NetworkPlanner;
use jiji_ssh::SshSession;
use jiji_tui::Ui;

use crate::ssh_adapter;

pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    hosts: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    let start = std::env::current_dir()?;
    let (config, _) =
        crate::config_loading::load_config_for_ssh(environment, config_file.map(Path::new), &start)
            .await?;
    if !validate_config(&config).valid {
        anyhow::bail!("Configuration is invalid; fix it before reading diagnostics");
    }
    let ssh = config
        .ssh
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No `ssh:` section is configured"))?;
    let plan = NetworkPlanner::new().plan(&config)?;
    let selected = plan.select_hosts(&split(hosts))?;
    if selected.is_empty() {
        anyhow::bail!("No servers are configured");
    }
    if !json {
        Ui::section("Control-Plane Diagnostics:");
    }
    let diag_started = std::time::Instant::now();
    let diag_hosts: Vec<String> = if !json {
        selected.iter().map(|s| s.name.clone()).collect()
    } else {
        Vec::new()
    };
    let diag_progress = if !json && !diag_hosts.is_empty() {
        let p =
            jiji_tui::ServerSetupProgress::with_title(diag_hosts.clone(), "Checking".to_string());
        Some(p)
    } else {
        None
    };
    let diag_handle = diag_progress.as_ref().map(|p| p.handle());
    let mut failures = Vec::new();
    for server_plan in selected {
        if let Some(h) = &diag_handle {
            h.set_status(&server_plan.name, "checking");
        }
        let name = &server_plan.name;
        let options = ssh_adapter::connect_options(name, &config.servers[name], ssh)?;
        let session = match SshSession::connect(&options).await {
            Ok(session) => session,
            Err(error) => {
                if let Some(h) = &diag_handle {
                    h.mark_failed(name, &error.to_string());
                }
                failures.push(format!("{name}: {error}"));
                continue;
            }
        };
        let response =
            crate::agent_client::call(&session, &config.project, None, RequestBody::Diagnostics)
                .await;
        session.close().await;
        match response {
            Ok(response) if json => {
                println!(
                    "{}",
                    serde_json::json!({"server": name, "diagnostics": response})
                );
            }
            Ok(ResponseBody::Diagnostics {
                schema_version,
                observation_count,
                uptime_seconds,
                database_usage_bytes,
                database_soft_quota_bytes,
                operation_counts,
                peer_reachability_timeout_secs,
                peer_sync,
                components,
                ..
            }) => {
                let quota = database_soft_quota_bytes
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "unlimited".into());
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let stale_peers = peer_sync
                    .iter()
                    .filter(|peer| {
                        peer.last_success_at
                            .as_deref()
                            .and_then(|value| value.parse::<u64>().ok())
                            .is_none_or(|last| {
                                now.saturating_sub(last) > peer_reachability_timeout_secs
                            })
                    })
                    .collect::<Vec<_>>();
                let summary = format!(
                        "schema={schema_version} uptime={uptime_seconds}s observations={observation_count} \
                         db={database_usage_bytes}/{quota}B operations={}/{}/{} peers={} components={}",
                        operation_counts.membership,
                        operation_counts.catalog,
                        operation_counts.desired,
                        peer_sync.len(),
                        components.len(),
                    );
                let unhealthy = !stale_peers.is_empty()
                    || peer_sync.iter().any(|peer| peer.consecutive_failures > 0)
                    || components
                        .iter()
                        .any(|component| component.consecutive_failures > 0);
                if unhealthy {
                    Ui::result_warn(name, &summary);
                } else {
                    Ui::result_ok(name, &summary);
                }
                for peer in stale_peers {
                    let age = peer
                        .last_success_at
                        .as_deref()
                        .and_then(|value| value.parse::<u64>().ok())
                        .map(|last| format!("{}s", now.saturating_sub(last)))
                        .unwrap_or_else(|| "never".into());
                    Ui::say(
                        &format!(
                            "{name}: peer {} is stale (last success {age} ago; DNS suppresses its services after {peer_reachability_timeout_secs}s)",
                            peer.node_id,
                        ),
                        2,
                    );
                }
                for peer in peer_sync
                    .iter()
                    .filter(|peer| peer.consecutive_failures > 0)
                {
                    Ui::say(
                        &format!(
                            "{name}: peer {} failures={} last_error={}",
                            peer.node_id,
                            peer.consecutive_failures,
                            peer.last_error.as_deref().unwrap_or("unknown")
                        ),
                        2,
                    );
                }
                for component in components
                    .iter()
                    .filter(|component| component.consecutive_failures > 0)
                {
                    Ui::say(
                        &format!(
                            "{name}: {} failures={} last_error={}",
                            component.component,
                            component.consecutive_failures,
                            component.last_error.as_deref().unwrap_or("unknown")
                        ),
                        2,
                    );
                }
            }
            Ok(other) => {
                if let Some(h) = &diag_handle {
                    h.mark_failed(name, "unexpected response");
                }
                failures.push(format!("{name}: unexpected response {other:?}"))
            }
            Err(error) => {
                if let Some(h) = &diag_handle {
                    h.mark_failed(name, &error.to_string());
                }
                failures.push(format!("{name}: {error}"))
            }
        }
        // For the ok path, also mark dashboard: success/warn based on unhealthy already rendered.
        // We re-derive quickly: if we reached the Ok(ResponseBody::Diagnostics) branch with result_ok,
        // that already implied success path; we mark there via side effect above.
        // To keep it simple, mark success if not already failed and not json.
        if !json {
            if let Some(h) = &diag_handle {
                // avoid double-mark: check if bar already finished? we use set logic: only mark if not yet marked failed
                // we track via failures length: if no new failure added for this host, mark success
                if !failures.iter().any(|f| f.starts_with(&format!("{name}:"))) {
                    // unhealthy still counts as warn but dashboard success keeps green check; result_warn line already printed
                    h.mark_success(name, "checked");
                }
            }
        }
    }
    if let Some(p) = diag_progress {
        p.finish();
    }
    if !json && !diag_hosts.is_empty() {
        Ui::say(
            &format!(
                "Checked {} host(s) in {}",
                diag_hosts.len(),
                jiji_tui::format_duration(diag_started.elapsed())
            ),
            1,
        );
    }
    for failure in &failures {
        Ui::result_warn("unavailable", failure);
    }
    if !failures.is_empty() {
        anyhow::bail!("Diagnostics failed on {} server(s)", failures.len());
    }
    Ok(())
}

fn split(value: Option<&str>) -> Vec<String> {
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
