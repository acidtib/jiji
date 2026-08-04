use serde::Serialize;

use jiji_tui::Ui;

use super::{close_all, connect_targets, discover_all};
use crate::lock;

#[derive(Serialize)]
struct LockStatus {
    scope: String,
    message: String,
    acquired_by: String,
    acquired_at: u64,
    pid: u32,
}

#[derive(Serialize)]
struct HostStatus {
    host: String,
    locked: bool,
    locks: Vec<LockStatus>,
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
    let statuses = discover_all(&targets).await?;
    close_all(&targets.sessions).await;

    if json {
        let hosts_json: Vec<HostStatus> = statuses
            .iter()
            .map(|(name, locks)| HostStatus {
                host: name.clone(),
                locked: !locks.is_empty(),
                locks: locks
                    .iter()
                    .map(|(scope, info)| LockStatus {
                        scope: scope.to_string(),
                        message: info.message.clone(),
                        acquired_by: info.acquired_by.clone(),
                        acquired_at: info.acquired_at,
                        pid: info.pid,
                    })
                    .collect(),
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
        .filter(|(_, locks)| !locks.is_empty())
        .collect();
    if locked.is_empty() {
        Ui::say("No active locks.", 1);
    } else {
        Ui::say(&format!("{} host(s) locked:", locked.len()), 1);
        for (name, locks) in &locked {
            for (scope, info) in locks {
                Ui::say(
                    &format!(
                        "{name} [{scope}]: \"{}\" by {} ({} ago)",
                        info.message,
                        info.acquired_by,
                        lock::format_age(info.age_seconds())
                    ),
                    2,
                );
            }
        }
    }

    let unlocked: Vec<&String> = statuses
        .iter()
        .filter(|(_, locks)| locks.is_empty())
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
