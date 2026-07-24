use serde::Serialize;

use jiji_tui::Ui;

use super::{close_all, connect_targets, read_all};
use crate::lock;

#[derive(Serialize)]
struct HostStatus {
    host: String,
    locked: bool,
    message: Option<String>,
    acquired_by: Option<String>,
    acquired_at: Option<u64>,
    pid: Option<u32>,
}

pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    hosts: Option<&str>,
    services: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    if !json {
        Ui::section("Lock Status:");
        Ui::section("Connecting:");
    }
    let targets = connect_targets(environment, config_file, hosts, services, json).await?;
    let statuses = read_all(&targets).await?;
    close_all(&targets.sessions).await;

    if json {
        let hosts_json: Vec<HostStatus> = statuses
            .iter()
            .map(|(name, info)| HostStatus {
                host: name.clone(),
                locked: info.is_some(),
                message: info.as_ref().map(|info| info.message.clone()),
                acquired_by: info.as_ref().map(|info| info.acquired_by.clone()),
                acquired_at: info.as_ref().map(|info| info.acquired_at),
                pid: info.as_ref().map(|info| info.pid),
            })
            .collect();
        let locked = hosts_json.iter().filter(|host| host.locked).count();
        let payload = serde_json::json!({
            "hosts": hosts_json,
            "summary": {
                "total": hosts_json.len(),
                "locked": locked,
                "unlocked": hosts_json.len() - locked,
            },
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    Ui::section("Status:");
    let locked: Vec<_> = statuses
        .iter()
        .filter_map(|(name, info)| info.as_ref().map(|info| (name, info)))
        .collect();
    if locked.is_empty() {
        Ui::say("No active deployment locks.", 1);
    } else {
        Ui::say(&format!("{} host(s) locked:", locked.len()), 1);
        for (name, info) in &locked {
            Ui::say(
                &format!(
                    "{name}: \"{}\" by {} ({} ago)",
                    info.message,
                    info.acquired_by,
                    lock::format_age(info.age_seconds())
                ),
                2,
            );
        }
    }

    let unlocked: Vec<&String> = statuses
        .iter()
        .filter(|(_, info)| info.is_none())
        .map(|(name, _)| name)
        .collect();
    if !unlocked.is_empty() {
        Ui::say(
            &format!(
                "Unlocked hosts: {}",
                unlocked
                    .iter()
                    .map(|name| name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            1,
        );
    }

    Ok(())
}
