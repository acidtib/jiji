use std::time::{SystemTime, UNIX_EPOCH};

use jiji_ssh::SshSession;
use serde::{Deserialize, Serialize};

use crate::env_resolution::project_staging_dir;

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
}

impl LockInfo {
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

/// Reads the lock file, if any. An unparsable lock file (corrupted, or written by an incompatible
/// future version) is treated as unlocked rather than as an error, so a bad lock file can never
/// permanently jam deploys -- `jiji lock acquire` will simply overwrite it.
pub async fn read_lock(session: &SshSession, project: &str) -> anyhow::Result<Option<LockInfo>> {
    let path = lock_file_path(project);
    let command = format!("cat {path} 2>/dev/null || true");
    let result = session.execute(&command).await?;
    let trimmed = result.stdout.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    Ok(serde_json::from_str::<LockInfo>(trimmed).ok())
}

/// Writes the lock file atomically (write to a temp path, then `mv`), the same pattern
/// `env_resolution::stage_env_file` uses, so a reader never observes a partially written file.
pub async fn write_lock(
    session: &SshSession,
    project: &str,
    info: &LockInfo,
) -> anyhow::Result<()> {
    let path = lock_file_path(project);
    let temp = format!("{path}.jiji-new");
    let content = serde_json::to_string_pretty(info)?;
    let command = format!("set -eu; install -D -m 0600 /dev/stdin {temp}; mv {temp} {path}");
    let result = session
        .execute_with_input(&command, content.as_bytes())
        .await?;
    if !result.success {
        anyhow::bail!(
            "Could not write lock file on {}: {}",
            session.host(),
            result.stderr.trim()
        );
    }
    Ok(())
}

pub async fn remove_lock(session: &SshSession, project: &str) -> anyhow::Result<()> {
    let path = lock_file_path(project);
    let result = session.execute(&format!("rm -f {path}")).await?;
    if !result.success {
        anyhow::bail!(
            "Could not remove lock file on {}: {}",
            session.host(),
            result.stderr.trim()
        );
    }
    Ok(())
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
        };
        assert_eq!(info.age_seconds(), 0);
    }

    #[test]
    fn format_age_covers_seconds_minutes_and_hours() {
        assert_eq!(format_age(5), "5s");
        assert_eq!(format_age(65), "1m5s");
        assert_eq!(format_age(3725), "1h2m");
    }
}
