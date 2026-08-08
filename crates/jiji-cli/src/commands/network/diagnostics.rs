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
    let mut failures = Vec::new();
    for server_plan in selected {
        let name = &server_plan.name;
        let options = ssh_adapter::connect_options(name, &config.servers[name], ssh)?;
        let session = match SshSession::connect(&options).await {
            Ok(session) => session,
            Err(error) => {
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
            Ok(other) => failures.push(format!("{name}: unexpected response {other:?}")),
            Err(error) => failures.push(format!("{name}: {error}")),
        }
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
