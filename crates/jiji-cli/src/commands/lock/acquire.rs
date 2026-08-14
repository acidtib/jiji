use std::time::{Duration, Instant};

use jiji_tui::Ui;

use super::{close_all, connect_targets, read_all};
use crate::audit::{self, AuditStatus};
use crate::lock::{self, LockInfo, LockScope};

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
    let started_at = Instant::now();

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
    let lock_hosts: Vec<String> = targets.sessions.keys().cloned().collect();
    let lk_progress =
        jiji_tui::ServerSetupProgress::with_title(lock_hosts.clone(), "Locking".to_string());
    let lk_handle = lk_progress.handle();
    for h in &lock_hosts {
        lk_handle.set_status(h, "locking");
    }
    if force {
        let operations: Vec<_> = targets
            .sessions
            .values()
            .cloned()
            .map(|session| {
                let path = LockScope::ProjectMaintenance.lock_path(&targets.project);
                move || async move { lock::force_remove_lock(&session, &path).await }
            })
            .collect();
        for result in targets.pool.execute_concurrent(operations).await {
            result?;
        }
    }

    let info = LockInfo::new(message);
    let lock_id = info
        .lock_id
        .as_deref()
        .expect("new locks have an ID")
        .to_string();

    let names: Vec<String> = targets.sessions.keys().cloned().collect();
    let operations: Vec<_> = names
        .iter()
        .map(|name| targets.sessions.get(name).expect("connected above").clone())
        .map(|session| {
            let path = LockScope::ProjectMaintenance.lock_path(&targets.project);
            let info = info.clone();
            move || async move { lock::acquire_lock(&session, &path, &info).await }
        })
        .collect();
    let results = targets.pool.execute_concurrent(operations).await;

    let mut acquired = Vec::new();
    let mut failures = Vec::new();
    for (name, result) in names.iter().zip(results) {
        match result {
            Ok(lock::AcquireResult::Acquired) => {
                lk_handle.mark_success(name, "acquired");
                Ui::result_ok(name, "lock acquired");
                acquired.push(name.clone());
            }
            Ok(lock::AcquireResult::Held(info)) => {
                lk_handle.mark_failed(name, "held");
                if let Some(info) = info {
                    Ui::result_error(
                        name,
                        &format!("held by {} (\"{}\")", info.acquired_by, info.message),
                    );
                } else {
                    Ui::result_error(name, "held but metadata incomplete");
                }
                failures.push(name.clone());
            }
            Err(error) => {
                lk_handle.mark_failed(name, &error.to_string());
                Ui::result_error(name, &error.to_string());
                failures.push(name.clone());
            }
        }
    }
    lk_progress.finish();

    if !failures.is_empty() {
        Ui::section("Rolling Back Partial Locks:");
        for name in &acquired {
            let session = targets.sessions.get(name).expect("connected above");
            let path = LockScope::ProjectMaintenance.lock_path(&targets.project);
            match lock::release_owned_lock(session, &path, &lock_id).await {
                Ok(lock::ReleaseOwnedResult::Released) => {
                    Ui::say(&format!("{name}: rolled back"), 1)
                }
                Ok(lock::ReleaseOwnedResult::NoLongerOwned) => Ui::warn(&format!(
                    "{name}: lock is no longer owned by this invocation; it was not removed"
                )),
                // Unlike `with_locks`'s own release path (see `lock.rs::release_requests`), this
                // rollback runs immediately after this same invocation successfully acquired the
                // lock -- nothing legitimate should have deleted its directory in between, so
                // this is worth surfacing, not silently ignoring.
                Ok(lock::ReleaseOwnedResult::AlreadyGone) => Ui::warn(&format!(
                    "{name}: lock directory is unexpectedly missing; nothing was rolled back"
                )),
                Err(error) => Ui::error(&format!("{name}: could not roll back ({error})")),
            }
        }
        close_all(&targets.sessions).await;
        anyhow::bail!(
            "Could not acquire lock on server(s): {}. No lock was left held on any server.",
            failures.join(", ")
        );
    }

    for name in &acquired {
        let session = targets.sessions.get(name).expect("connected above");
        audit::record(
            session,
            &targets.project,
            "lock_acquire",
            AuditStatus::Success,
            format!("\"{message}\" by {}", lock::current_user()),
            Some(&LockScope::ProjectMaintenance.to_string()),
            None,
            Some(started_at.elapsed()),
        )
        .await;
    }
    close_all(&targets.sessions).await;
    Ui::success("\nDeployment lock acquired.");
    Ui::say(&format!("Message: {message}"), 1);
    Ui::say("To release: jiji lock release", 1);
    Ok(())
}
