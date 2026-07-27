use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jiji_ssh::SshSession;
use jiji_tui::Ui;
use serde::{Deserialize, Serialize};

use crate::env_resolution::project_staging_dir;

/// Audit entries live at `.jiji/{project}/audit.log` on each server, inside the same
/// `.jiji/{project}` staging directory `env_resolution::project_staging_dir` and
/// `crate::lock::lock_file_path` already use, relative to the SSH user's home directory. One
/// append-only, newline-delimited JSON file per project per server -- not a shared/central log,
/// mirroring the deployment lock's own per-server-per-project scoping.
fn audit_file_path(project: &str) -> String {
    format!("{}/audit.log", project_staging_dir(project))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuditStatus {
    Success,
    Failed,
}

impl fmt::Display for AuditStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuditStatus::Success => write!(f, "SUCCESS"),
            AuditStatus::Failed => write!(f, "FAILED"),
        }
    }
}

impl std::str::FromStr for AuditStatus {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "success" => Ok(AuditStatus::Success),
            "failed" => Ok(AuditStatus::Failed),
            other => anyhow::bail!("Unknown audit status '{other}'. Use 'success' or 'failed'."),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub timestamp: u64,
    pub action: String,
    pub status: AuditStatus,
    pub actor: String,
    pub message: String,
    /// How long the recorded operation took, in milliseconds. `None` for entries written before
    /// this field existed, or from a call site that doesn't track a start time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

impl AuditEntry {
    pub fn new(
        action: impl Into<String>,
        status: AuditStatus,
        message: impl Into<String>,
        duration: Option<Duration>,
    ) -> Self {
        Self {
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or(0),
            action: action.into(),
            status,
            actor: crate::lock::current_user(),
            message: message.into(),
            duration_ms: duration.map(|d| d.as_millis() as u64),
        }
    }
}

pub fn format_timestamp(timestamp: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let age = now.saturating_sub(timestamp);
    format!("{} ago", crate::lock::format_age(age))
}

/// Formats an operation duration for display: sub-second precision below a minute (deploys and
/// restarts typically land here), otherwise the same coarse `{m}m{s}s` shape `lock::format_age`
/// uses for lock ages.
pub fn format_duration_ms(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        crate::lock::format_age(ms / 1000)
    }
}

/// Appends one entry, creating the project's staging directory if this is the first entry on this
/// server. Content is piped over stdin rather than inlined into the command string (same reasoning
/// as `env_resolution::stage_env_file`: no value -- here, a caller-supplied
/// message or error string -- should ever appear embedded in a logged shell command). Appends are
/// not atomic across concurrent writers (unlike the lock file's write-temp-then-`mv`): two
/// operations racing to audit the same server at the same instant could interleave, which is an
/// accepted, low-probability risk for an observability side channel, not a correctness-critical
/// file -- the same class of accepted concurrent-invocation risk already documented for network
/// setup.
pub async fn append_entry(
    session: &SshSession,
    project: &str,
    entry: &AuditEntry,
) -> anyhow::Result<()> {
    let path = audit_file_path(project);
    let dir = project_staging_dir(project);
    let mut line = serde_json::to_string(entry)?;
    line.push('\n');
    let command = format!("set -eu; install -d -m 0700 {dir}; umask 077; cat >> {path}");
    let result = session
        .execute_with_input(&command, line.as_bytes())
        .await?;
    if !result.success {
        anyhow::bail!(
            "Could not append audit entry on {}: {}",
            session.host(),
            result.stderr.trim()
        );
    }
    Ok(())
}

/// Best-effort audit write: failures are warned, never propagated. Audit logging is observability,
/// not a correctness gate -- a write failure here must never mask, override, or block the outcome
/// of the command it's recording, so every call site can fire-and-forget this instead of threading
/// its own error handling through.
pub async fn record(
    session: &SshSession,
    project: &str,
    action: &str,
    status: AuditStatus,
    message: impl Into<String>,
    duration: Option<Duration>,
) {
    let entry = AuditEntry::new(action, status, message, duration);
    if let Err(error) = append_entry(session, project, &entry).await {
        Ui::warn(&format!(
            "Could not write audit entry ({action}) on {}: {error}",
            session.host()
        ));
    }
}

/// Groups `(identity, server, succeeded)` triples by server and writes one audit entry per server
/// via `record`, summarizing every endpoint touched on that server during this run -- the shared
/// tail end of `jiji deploy`/`service restart`/`service rollback`, which all drive the same
/// per-endpoint `deploy_transaction::deploy_endpoint` primitive and just differ in what image they
/// deploy. Must be called before the caller closes `sessions`: the write reuses the same SSH
/// session the command itself just used. A server with no session open (shouldn't happen -- every
/// endpoint's server was connected to reach this point) is silently skipped rather than panicking,
/// since a missing audit entry must never be treated as more severe than the command's own result.
pub async fn record_endpoints_by_server(
    sessions: &BTreeMap<String, Arc<SshSession>>,
    project: &str,
    action: &str,
    detail: Option<&str>,
    outcomes: impl IntoIterator<Item = (String, String, bool)>,
    duration: Option<Duration>,
) {
    let mut by_server: BTreeMap<String, Vec<(String, bool)>> = BTreeMap::new();
    for (identity, server, succeeded) in outcomes {
        by_server
            .entry(server)
            .or_default()
            .push((identity, succeeded));
    }
    for (server_name, endpoint_results) in &by_server {
        let Some(session) = sessions.get(server_name) else {
            continue;
        };
        let all_succeeded = endpoint_results.iter().all(|(_, ok)| *ok);
        let identities = endpoint_results
            .iter()
            .map(|(identity, ok)| format!("{identity}{}", if *ok { "" } else { " (failed)" }))
            .collect::<Vec<_>>()
            .join(", ");
        let summary = match detail {
            Some(detail) => format!("{detail}: {identities}"),
            None => identities,
        };
        record(
            session,
            project,
            action,
            if all_succeeded {
                AuditStatus::Success
            } else {
                AuditStatus::Failed
            },
            summary,
            duration,
        )
        .await;
    }
}

/// Reads up to the last `tail` entries on this server. Malformed or partial lines (e.g. from a
/// concurrent-write race, or a future incompatible format) are silently skipped rather than
/// failing the read, the same resilience `lock::read_lock` applies to a corrupted lock file --
/// a bad audit line must never make the audit trail itself unreadable.
pub async fn read_entries(
    session: &SshSession,
    project: &str,
    tail: u32,
) -> anyhow::Result<Vec<AuditEntry>> {
    let path = audit_file_path(project);
    let n = tail.max(1);
    let command = format!("tail -n {n} {path} 2>/dev/null || true");
    let result = session.execute(&command).await?;
    Ok(result
        .stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<AuditEntry>(line.trim()).ok())
        .collect())
}

/// Reads the full live audit log for aggregation. Stats intentionally do not use the listing
/// command's tail cutoff: applying a time window after `tail -n` could silently omit entries that
/// belong in the window on a busy server.
pub async fn read_all_entries(
    session: &SshSession,
    project: &str,
) -> anyhow::Result<Vec<AuditEntry>> {
    let path = audit_file_path(project);
    let command = format!("cat {path} 2>/dev/null || true");
    let result = session.execute(&command).await?;
    Ok(result
        .stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<AuditEntry>(line.trim()).ok())
        .collect())
}

/// The remote command `jiji audit --follow` streams via `stream_logs` -- plain `tail -f` on the
/// raw JSONL file, deliberately not reformatted server-side (no reliable `jq` to depend on, and
/// reformatting a byte stream client-side line-by-line isn't worth the complexity for a follow
/// mode). `--follow` output is always raw JSON lines, regardless of `--json`.
pub fn render_follow_command(project: &str) -> String {
    let path = audit_file_path(project);
    format!(
        "install -d -m 0700 {}; touch {path}; tail -n 0 -f {path}",
        project_staging_dir(project)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_file_path_is_scoped_to_the_project() {
        assert_eq!(audit_file_path("demo"), ".jiji/demo/audit.log");
    }

    #[test]
    fn status_round_trips_through_its_string_form() {
        assert_eq!(
            "success".parse::<AuditStatus>().unwrap(),
            AuditStatus::Success
        );
        assert_eq!(
            "FAILED".parse::<AuditStatus>().unwrap(),
            AuditStatus::Failed
        );
        assert!("bogus".parse::<AuditStatus>().is_err());
    }

    #[test]
    fn entry_serializes_to_a_single_json_line_with_no_embedded_newline() {
        let entry = AuditEntry::new(
            "deploy",
            AuditStatus::Success,
            "demo:web:app deployed",
            Some(Duration::from_millis(1234)),
        );
        let json = serde_json::to_string(&entry).unwrap();
        assert!(!json.contains('\n'));
        let parsed: AuditEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.action, "deploy");
        assert_eq!(parsed.status, AuditStatus::Success);
        assert_eq!(parsed.duration_ms, Some(1234));
    }

    #[test]
    fn entries_written_before_duration_existed_still_parse() {
        let legacy =
            r#"{"timestamp":1,"action":"deploy","status":"success","actor":"x","message":"m"}"#;
        let parsed: AuditEntry = serde_json::from_str(legacy).unwrap();
        assert_eq!(parsed.duration_ms, None);
    }

    #[test]
    fn duration_formatting_switches_precision_by_magnitude() {
        assert_eq!(format_duration_ms(850), "850ms");
        assert_eq!(format_duration_ms(12_400), "12.4s");
        assert_eq!(format_duration_ms(59_999), "60.0s");
        assert_eq!(format_duration_ms(60_000), "1m0s");
        assert_eq!(format_duration_ms(125_000), "2m5s");
    }

    #[test]
    fn format_timestamp_reports_an_age_suffix() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(format_timestamp(now), "0s ago");
    }
}
