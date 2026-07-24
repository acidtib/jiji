use jiji_tui::Ui;

use super::{close_all, connect_targets, read_all};
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
    let statuses = read_all(&targets).await?;
    close_all(&targets.sessions).await;

    Ui::section("Hosts:");
    for (name, info) in &statuses {
        Ui::say(&format!("{name}:"), 1);
        match info {
            Some(info) => {
                Ui::say("Status: LOCKED", 2);
                Ui::say(&format!("Message: {}", info.message), 2);
                Ui::say(&format!("Acquired by: {}", info.acquired_by), 2);
                Ui::say(
                    &format!("Acquired: {} ago", lock::format_age(info.age_seconds())),
                    2,
                );
                Ui::say(&format!("Process ID: {}", info.pid), 2);
            }
            None => {
                Ui::say("Status: UNLOCKED", 2);
                Ui::say("Available for deployment", 2);
            }
        }
    }

    Ok(())
}
