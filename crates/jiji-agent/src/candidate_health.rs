//! Local-only replay of a candidate's deploy-time health check.
//!
//! `jiji-agent` has no access to a service's configured `healthcheck:`; that logic lives in
//! `jiji-cli`'s `health_check.rs`. Adding it to the replicated `CatalogRecord` would force a
//! `CATALOG_SCHEMA_VERSION` bump and break mesh replication mid-rollout, so `jiji-cli` records the
//! rendered `HealthCheckPlan` command here instead, in a local-only table (same precedent as
//! `ImageRetentionSpec`/`CronJobSpec`). `recover_startup_candidates` replays it before trusting an
//! orphaned `Candidate` enough to promote it. The command is the same string the CLI would
//! otherwise run over SSH, so replaying it locally (the agent already runs as root on this host)
//! carries no new trust boundary.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::process::Command;

/// One recorded deploy-time health check for a still-`Candidate` deployment, keyed by
/// `deployment_id`. `interval_secs` is unused by `verify_locally` (one attempt per call, not a
/// poll loop); kept for parity with the CLI's own `HealthCheckPlan`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateHealthCheckSpec {
    pub deployment_id: String,
    pub service: String,
    pub replica_id: String,
    pub command: String,
    pub interval_secs: u64,
    pub deploy_timeout_secs: u64,
}

/// Runs `spec.command` via a local shell exactly once, bounded by `deploy_timeout_secs`. Not a
/// poll-until-success loop: `reconcile_once`'s own tick cadence is already the retry mechanism.
/// The bound only guards against a hung command (an HTTP check already self-bounds via its own
/// `curl --max-time`; a `cmd` check has no such guarantee).
pub async fn verify_locally(spec: &CandidateHealthCheckSpec) -> Result<(), String> {
    let bound = Duration::from_secs(spec.deploy_timeout_secs.max(1));
    let attempt = Command::new("sh").arg("-c").arg(&spec.command).output();
    match tokio::time::timeout(bound, attempt).await {
        Ok(Ok(output)) if output.status.success() => Ok(()),
        Ok(Ok(output)) => Err(summarize_failure(&output)),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err(format!(
            "health check did not complete within {}s",
            bound.as_secs()
        )),
    }
}

fn summarize_failure(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    if let Some(line) = stderr.lines().find(|line| !line.trim().is_empty()) {
        return line.trim().to_string();
    }
    if let Some(line) = stdout.lines().find(|line| !line.trim().is_empty()) {
        return line.trim().to_string();
    }
    match output.status.code() {
        Some(code) => format!("exited with status {code}"),
        None => "no exit status (killed by signal)".to_string(),
    }
}
