use std::time::{Duration, Instant};

use jiji_tui::Ui;

use super::{close_all, connect_targets, read_all};
use crate::lock::{self, LockInfo};

pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    hosts: Option<&str>,
    services: Option<&str>,
    message: &str,
    timeout: u64,
    force: bool,
) -> anyhow::Result<()> {
    Ui::section("Lock Acquire:");
    Ui::say(&format!("Message: {message}"), 1);

    Ui::section("Connecting:");
    let targets = connect_targets(environment, config_file, hosts, services, false).await?;

    Ui::section("Checking Existing Locks:");
    let deadline = Instant::now() + Duration::from_secs(timeout);
    let mut warned = false;
    let locked = loop {
        let statuses = read_all(&targets).await?;
        let locked: Vec<(String, LockInfo)> = statuses
            .into_iter()
            .filter_map(|(name, info)| info.map(|info| (name, info)))
            .collect();
        if locked.is_empty() || force {
            break locked;
        }
        if Instant::now() >= deadline {
            close_all(&targets.sessions).await;
            let mut detail = String::new();
            for (name, info) in &locked {
                detail.push_str(&format!(
                    "\n  {name}: \"{}\" by {} ({} ago)",
                    info.message,
                    info.acquired_by,
                    lock::format_age(info.age_seconds())
                ));
            }
            anyhow::bail!(
                "Timed out after {timeout}s waiting for the deployment lock to free up:{detail}\nUse `jiji lock status` to inspect, or pass `--force` to override.",
            );
        }
        if !warned {
            Ui::warn(&format!(
                "{} host(s) already locked; waiting up to {timeout}s for release...",
                locked.len()
            ));
            warned = true;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    };

    if force && !locked.is_empty() {
        for (name, info) in &locked {
            Ui::warn(&format!(
                "{name}: overriding existing lock (\"{}\" by {})",
                info.message, info.acquired_by
            ));
        }
    }

    Ui::section("Creating Lock Files:");
    let info = LockInfo {
        message: message.to_string(),
        acquired_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock should be after the unix epoch")
            .as_secs(),
        acquired_by: lock::current_user(),
        pid: std::process::id(),
    };

    let names: Vec<String> = targets.sessions.keys().cloned().collect();
    let operations: Vec<_> = names
        .iter()
        .map(|name| targets.sessions.get(name).expect("connected above").clone())
        .map(|session| {
            let project = targets.project.clone();
            let info = info.clone();
            move || async move { lock::write_lock(&session, &project, &info).await }
        })
        .collect();
    let results = targets.pool.execute_concurrent(operations).await;

    let mut acquired = Vec::new();
    let mut failures = Vec::new();
    for (name, result) in names.iter().zip(results) {
        match result {
            Ok(()) => {
                Ui::say(&format!("{name}: lock acquired"), 1);
                acquired.push(name.clone());
            }
            Err(error) => {
                Ui::error(&format!("{name}: {error}"));
                failures.push(name.clone());
            }
        }
    }

    if !failures.is_empty() {
        Ui::section("Rolling Back Partial Locks:");
        for name in &acquired {
            let session = targets.sessions.get(name).expect("connected above");
            match lock::remove_lock(session, &targets.project).await {
                Ok(()) => Ui::say(&format!("{name}: rolled back"), 1),
                Err(error) => Ui::error(&format!("{name}: could not roll back ({error})")),
            }
        }
        close_all(&targets.sessions).await;
        anyhow::bail!(
            "Could not acquire lock on server(s): {}. No lock was left held on any server.",
            failures.join(", ")
        );
    }

    close_all(&targets.sessions).await;
    Ui::success("\nDeployment lock acquired.");
    Ui::say(&format!("Message: {message}"), 1);
    Ui::say("To release: jiji lock release", 1);
    Ok(())
}
