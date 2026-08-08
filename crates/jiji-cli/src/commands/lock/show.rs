use jiji_tui::Ui;

use super::{close_all, connect_targets, discover_all};
use crate::lock;

pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    hosts: Option<&str>,
    services: Option<&str>,
) -> anyhow::Result<()> {
    Ui::section("Lock Details:");

    Ui::section("Connecting:");
    let targets = connect_targets(environment, config_file, hosts, services, false).await?;
    let statuses = discover_all(&targets).await?;
    close_all(&targets.sessions).await;

    Ui::section("Hosts:");
    for (name, locks) in &statuses {
        Ui::say(&format!("{name}:"), 1);
        if locks.is_empty() {
            Ui::say("Status: UNLOCKED", 2);
            Ui::say("Available", 2);
            continue;
        }
        for (scope, info) in locks {
            Ui::say(&format!("Scope: {scope}"), 2);
            Ui::say("Status: LOCKED", 3);
            Ui::say(&format!("Message: {}", info.message), 3);
            Ui::say(&format!("Acquired by: {}", info.acquired_by), 3);
            Ui::say(
                &format!("Acquired: {} ago", lock::format_age(info.age_seconds())),
                3,
            );
            Ui::say(&format!("Process ID: {}", info.pid), 3);
        }
    }

    Ok(())
}
