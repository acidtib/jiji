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

    let started = std::time::Instant::now();
    Ui::section("Hosts:");
    for (name, locks) in &statuses {
        if locks.is_empty() {
            Ui::result_ok(name, "UNLOCKED — Available");
            continue;
        }
        Ui::say(&format!("{name}:"), 1);
        for (scope, info) in locks {
            Ui::result_warn(
                &format!("{name} [{scope}]"),
                &format!(
                    "LOCKED \"{}\" by {} ({} ago, pid {})",
                    info.message,
                    info.acquired_by,
                    lock::format_age(info.age_seconds()),
                    info.pid
                ),
            );
        }
    }
    Ui::say(
        &format!(
            "Inspected {} host(s) in {}",
            statuses.len(),
            jiji_tui::format_duration(started.elapsed())
        ),
        1,
    );

    Ok(())
}
