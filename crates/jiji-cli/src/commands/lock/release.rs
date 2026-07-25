use jiji_tui::Ui;

use super::{close_all, connect_targets, read_all};
use crate::audit::{self, AuditStatus};
use crate::lock;

pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    hosts: Option<&str>,
    services: Option<&str>,
) -> anyhow::Result<()> {
    Ui::section("Lock Release:");
    let started_at = std::time::Instant::now();

    Ui::section("Connecting:");
    let targets = connect_targets(environment, config_file, hosts, services, false).await?;

    Ui::section("Checking Existing Locks:");
    let statuses = read_all(&targets).await?;
    let locked_count = statuses.iter().filter(|(_, info)| info.is_some()).count();
    if locked_count == 0 {
        Ui::warn("No deployment locks found.");
        close_all(&targets.sessions).await;
        return Ok(());
    }

    Ui::section("Removing Lock Files:");
    let names: Vec<String> = targets.sessions.keys().cloned().collect();
    let operations: Vec<_> = names
        .iter()
        .map(|name| targets.sessions.get(name).expect("connected above").clone())
        .map(|session| {
            let project = targets.project.clone();
            move || async move { lock::remove_lock(&session, &project).await }
        })
        .collect();
    let results = targets.pool.execute_concurrent(operations).await;

    let mut failures = Vec::new();
    for (name, result) in names.iter().zip(results) {
        match result {
            Ok(()) => {
                Ui::say(&format!("{name}: lock released"), 1);
                let session = targets.sessions.get(name).expect("connected above");
                audit::record(
                    session,
                    &targets.project,
                    "lock_release",
                    AuditStatus::Success,
                    format!("released by {}", lock::current_user()),
                    Some(started_at.elapsed()),
                )
                .await;
            }
            Err(error) => {
                Ui::error(&format!("{name}: {error}"));
                failures.push(name.clone());
            }
        }
    }
    close_all(&targets.sessions).await;

    if !failures.is_empty() {
        anyhow::bail!(
            "Could not release lock on server(s): {}. Retry `jiji lock release` for those hosts.",
            failures.join(", ")
        );
    }

    Ui::success("\nDeployment lock released.");
    Ok(())
}
