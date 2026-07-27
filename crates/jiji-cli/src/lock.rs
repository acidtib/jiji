use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use jiji_ssh::{SshPool, SshSession};
use serde::{Deserialize, Serialize};

use crate::env_resolution::project_staging_dir;

static LOCK_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Deployment locks live at `.jiji/{project}/deploy.lock` on each server, inside the same
/// `.jiji/{project}` staging directory `env_resolution::project_staging_dir` already uses for
/// uploaded env files, relative to the SSH user's home directory.
fn lock_file_path(project: &str) -> String {
    format!("{}/deploy.lock", project_staging_dir(project))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockInfo {
    pub message: String,
    pub acquired_at: u64,
    pub acquired_by: String,
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_id: Option<String>,
}

impl LockInfo {
    pub fn new(message: impl Into<String>) -> Self {
        let acquired_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after the unix epoch");
        let pid = std::process::id();
        Self {
            message: message.into(),
            acquired_at: acquired_at.as_secs(),
            acquired_by: current_user(),
            pid,
            lock_id: Some(format!(
                "{pid}-{}-{}",
                acquired_at.as_nanos(),
                LOCK_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            )),
        }
    }

    pub fn age_seconds(&self) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        now.saturating_sub(self.acquired_at)
    }
}

/// The local operator's username, used as `acquired_by` -- read locally rather than over SSH so
/// it reflects who ran `jiji lock acquire`, not the (often shared/service-account) SSH login user.
pub fn current_user() -> String {
    if let Ok(output) = std::process::Command::new("whoami").output() {
        if output.status.success() {
            let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !name.is_empty() {
                return name;
            }
        }
    }
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}

pub fn format_age(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m{}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h{}m", seconds / 3600, (seconds % 3600) / 60)
    }
}

/// Reads either the directory-backed atomic lock or the legacy lock file. Missing or malformed
/// metadata is treated as unlocked so interrupted writes cannot permanently jam deployments.
pub async fn read_lock(session: &SshSession, project: &str) -> anyhow::Result<Option<LockInfo>> {
    let path = lock_file_path(project);
    let result = session
        .execute(&format!("cat {path}/info.json 2>/dev/null || true"))
        .await?;
    let mut trimmed = result.stdout.trim();
    let legacy;
    if trimmed.is_empty() {
        legacy = session
            .execute(&format!("cat {path} 2>/dev/null || true"))
            .await?;
        trimmed = legacy.stdout.trim();
    }
    if let Ok(info) = serde_json::from_str::<LockInfo>(trimmed) {
        return Ok(Some(info));
    }
    Ok(None)
}

#[derive(Debug)]
pub enum AcquireResult {
    Acquired,
    Held(Option<LockInfo>),
}

/// Claims the lock with `mkdir`, whose existence check and creation are one atomic filesystem
/// operation. Contention is reported through a stable stdout marker so it remains distinguishable
/// from transport and filesystem failures.
pub async fn acquire_lock(
    session: &SshSession,
    project: &str,
    info: &LockInfo,
) -> anyhow::Result<AcquireResult> {
    let path = lock_file_path(project);
    let content = serde_json::to_string_pretty(info)?;
    let lock_id = info.lock_id.as_deref().expect("new locks have an ID");
    let pending = format!("{path}.{lock_id}.pending");
    let command = format!(
        "set -eu\n\
         mkdir -p {}\n\
         mkdir {pending}\n\
         if ! install -m 0600 /dev/stdin {pending}/info.json; then rmdir {pending}; exit 74; fi\n\
         if ! mv -T {pending} {path} 2>/dev/null; then\n\
           rm -f {pending}/info.json\n\
           rmdir {pending}\n\
           echo JIJI_LOCK_HELD\n\
         fi",
        project_staging_dir(project)
    );
    let mut result = session
        .execute_with_input(&command, content.as_bytes())
        .await?;
    if result.success && result.stdout.trim() == "JIJI_LOCK_HELD" {
        if let Some(holder) = read_lock(session, project).await? {
            return Ok(AcquireResult::Held(Some(holder)));
        }

        recover_incomplete_lock(session, project).await?;
        result = session
            .execute_with_input(&command, content.as_bytes())
            .await?;
        if result.success && result.stdout.trim() == "JIJI_LOCK_HELD" {
            return Ok(AcquireResult::Held(read_lock(session, project).await?));
        }
    }
    if result.success {
        return Ok(AcquireResult::Acquired);
    }
    anyhow::bail!(
        "Could not acquire deployment lock on {}: {}",
        session.host(),
        result.stderr.trim()
    )
}

async fn recover_incomplete_lock(session: &SshSession, project: &str) -> anyhow::Result<()> {
    let path = lock_file_path(project);
    let command = format!(
        "if [ -d {path} ]; then\n\
           rm -f {path}/info.json\n\
           rmdir {path} 2>/dev/null || true\n\
         elif [ -e {path} ]; then\n\
           rm -f {path}\n\
         fi"
    );
    let result = session.execute(&command).await?;
    if !result.success {
        anyhow::bail!(
            "Could not recover incomplete deployment lock on {}: {}",
            session.host(),
            result.stderr.trim()
        );
    }
    Ok(())
}

/// Removes a lock only when its unique ID still belongs to the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseOwnedResult {
    Released,
    NoLongerOwned,
}

pub async fn release_owned_lock(
    session: &SshSession,
    project: &str,
    lock_id: &str,
) -> anyhow::Result<ReleaseOwnedResult> {
    let path = lock_file_path(project);
    let expected = serde_json::to_string(lock_id)?;
    let command = format!(
        "set -eu\n\
         test -d {path} || exit 75\n\
         actual=$(sed -n 's/.*\"lock_id\"[[:space:]]*:[[:space:]]*\"\\([^\"]*\\)\".*/\\1/p' {path}/info.json)\n\
         test \"$actual\" = {expected} || exit 75\n\
         rm -f {path}/info.json\n\
         rmdir {path}"
    );
    let result = session.execute(&command).await?;
    if result.code == Some(75) {
        return Ok(ReleaseOwnedResult::NoLongerOwned);
    }
    if !result.success {
        anyhow::bail!(
            "Could not release deployment lock on {}: {}",
            session.host(),
            result.stderr.trim()
        );
    }
    Ok(ReleaseOwnedResult::Released)
}

pub async fn force_remove_lock(session: &SshSession, project: &str) -> anyhow::Result<()> {
    let path = lock_file_path(project);
    let command = format!(
        "if [ -d {path} ]; then rm -f {path}/info.json && rmdir {path}; else rm -f {path}; fi"
    );
    let result = session.execute(&command).await?;
    if !result.success {
        anyhow::bail!(
            "Could not remove deployment lock on {}: {}",
            session.host(),
            result.stderr.trim()
        );
    }
    Ok(())
}

pub struct OwnedDeploymentLocks {
    project: String,
    lock_id: String,
    hosts: Vec<String>,
}

impl OwnedDeploymentLocks {
    pub async fn acquire(
        pool: &SshPool,
        sessions: &BTreeMap<String, Arc<SshSession>>,
        project: &str,
        message: impl Into<String>,
        timeout: u64,
        force: bool,
    ) -> anyhow::Result<Self> {
        let info = LockInfo::new(message);
        let lock_id = info
            .lock_id
            .as_deref()
            .expect("new locks have an ID")
            .to_string();
        let names: Vec<String> = sessions.keys().cloned().collect();
        let deadline = Instant::now() + Duration::from_secs(timeout);
        let mut warned = false;
        loop {
            if force {
                let operations: Vec<_> = sessions
                    .values()
                    .cloned()
                    .map(|session| {
                        let project = project.to_string();
                        move || async move { force_remove_lock(&session, &project).await }
                    })
                    .collect();
                for result in pool.execute_concurrent(operations).await {
                    result?;
                }
            }
            let operations: Vec<_> = names
                .iter()
                .map(|name| sessions.get(name).expect("host has a session").clone())
                .map(|session| {
                    let project = project.to_string();
                    let info = info.clone();
                    move || async move { acquire_lock(&session, &project, &info).await }
                })
                .collect();
            let results = pool.execute_concurrent(operations).await;

            let mut acquired = Vec::new();
            let mut failures = Vec::new();
            let mut hard_failure = false;
            for (name, result) in names.iter().zip(results) {
                match result {
                    Ok(AcquireResult::Acquired) => acquired.push(name.clone()),
                    Ok(AcquireResult::Held(Some(holder))) => failures.push(format!(
                        "{name}: \"{}\" by {} ({} ago)",
                        holder.message,
                        holder.acquired_by,
                        format_age(holder.age_seconds())
                    )),
                    Ok(AcquireResult::Held(None)) => {
                        failures.push(format!("{name}: lock metadata is incomplete"))
                    }
                    Err(error) => {
                        hard_failure = true;
                        failures.push(format!("{name}: {error}"));
                    }
                }
            }

            if failures.is_empty() {
                return Ok(Self {
                    project: project.to_string(),
                    lock_id,
                    hosts: acquired,
                });
            }
            let (rollback_warnings, rollback_errors) =
                release_hosts(pool, sessions, project, &lock_id, &acquired).await;
            let mut message = format!(
                "Could not acquire the deployment lock on every server:\n  {}",
                failures.join("\n  ")
            );
            if !rollback_errors.is_empty() {
                message.push_str(&format!(
                    "\nCould not roll back partial locks:\n  {}",
                    rollback_errors.join("\n  ")
                ));
            }
            for warning in rollback_warnings {
                jiji_tui::Ui::warn(&warning);
            }
            if hard_failure || !rollback_errors.is_empty() || Instant::now() >= deadline {
                message.push_str(
                    "\nCheck `jiji lock status`, and once it is safe, run `jiji lock release`.",
                );
                anyhow::bail!("{message}");
            }
            if !warned {
                jiji_tui::Ui::warn(&format!(
                    "Deployment lock is held; waiting up to {timeout}s for it to be released."
                ));
                warned = true;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    pub async fn release(
        self,
        pool: &SshPool,
        sessions: &BTreeMap<String, Arc<SshSession>>,
    ) -> anyhow::Result<()> {
        let (warnings, errors) =
            release_hosts(pool, sessions, &self.project, &self.lock_id, &self.hosts).await;
        for warning in warnings {
            jiji_tui::Ui::warn(&warning);
        }
        if !errors.is_empty() {
            anyhow::bail!(
                "Could not release deployment lock on server(s): {}. Run `jiji lock release` after verifying no deployment is active.",
                errors.join(", ")
            );
        }
        Ok(())
    }
}

async fn release_hosts(
    pool: &SshPool,
    sessions: &BTreeMap<String, Arc<SshSession>>,
    project: &str,
    lock_id: &str,
    hosts: &[String],
) -> (Vec<String>, Vec<String>) {
    let operations: Vec<_> = hosts
        .iter()
        .map(|name| {
            sessions
                .get(name)
                .expect("acquired host has a session")
                .clone()
        })
        .map(|session| {
            let project = project.to_string();
            let lock_id = lock_id.to_string();
            move || async move { release_owned_lock(&session, &project, &lock_id).await }
        })
        .collect();
    let mut warnings = Vec::new();
    let mut errors = Vec::new();
    for (name, result) in hosts.iter().zip(pool.execute_concurrent(operations).await) {
        match result {
            Ok(ReleaseOwnedResult::Released) => {}
            Ok(ReleaseOwnedResult::NoLongerOwned) => warnings.push(format!(
                "Deployment lock on {name} is no longer owned by this invocation; it was not removed."
            )),
            Err(error) => errors.push(format!("{name}: {error}")),
        }
    }
    (warnings, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_file_path_is_scoped_to_the_project() {
        assert_eq!(lock_file_path("demo"), ".jiji/demo/deploy.lock");
    }

    #[test]
    fn age_seconds_is_zero_for_a_lock_acquired_just_now() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let info = LockInfo {
            message: "test".to_string(),
            acquired_at: now,
            acquired_by: "alice".to_string(),
            pid: 1,
            lock_id: None,
        };
        assert_eq!(info.age_seconds(), 0);
    }

    #[test]
    fn format_age_covers_seconds_minutes_and_hours() {
        assert_eq!(format_age(5), "5s");
        assert_eq!(format_age(65), "1m5s");
        assert_eq!(format_age(3725), "1h2m");
    }

    #[test]
    fn legacy_lock_deserializes_without_an_ownership_id() {
        let info: LockInfo = serde_json::from_str(
            r#"{"message":"maintenance","acquired_at":1,"acquired_by":"alice","pid":2}"#,
        )
        .unwrap();
        assert_eq!(info.lock_id, None);
    }

    #[test]
    fn new_locks_have_distinct_ownership_ids() {
        let first = LockInfo::new("first");
        let second = LockInfo::new("second");
        assert!(first.lock_id.is_some());
        assert_ne!(first.lock_id, second.lock_id);
    }
}
