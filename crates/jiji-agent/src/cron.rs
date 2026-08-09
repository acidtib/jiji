//! Cron job specifications and run history (`docs/architecture-notes.md#ownership-and-reconciliation`):
//! durable local state for `jiji-agent`'s scheduled-command feature. Unlike membership/catalog/
//! desired-state, none of this is replicated between hosts -- only a job's assigned owner ever
//! needs it, so there is no `RecordProvenance`/anti-entropy machinery here.

use serde::{Deserialize, Serialize};

use crate::membership::content_hash;

/// `forbid` skips a due run while the prior run is still active. A single-variant enum (not
/// `bool`), mirroring `jiji_config::CronOverlap`, so a later release can add a variant without a
/// wire-format break. `jiji-agent` links `jiji-config` transitively (via `jiji-network`), but this
/// crate's own durable/wire types deliberately never reuse a CLI-facing config-schema type
/// directly -- `CatalogRecord`/`DesiredStateRecord` already follow the same rule -- so this is a
/// deliberate structural duplicate, not a shared type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CronOverlap {
    Forbid,
}

/// `skip` does not replay scheduled times missed while the owning agent was offline. Mirrors
/// `jiji_config::CronMissedRuns` for the same reason as `CronOverlap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CronMissedRuns {
    Skip,
}

/// One installed cron job specification, built by `jiji-cli` from a successful service deployment
/// and applied to the owning replica's agent (see the plan's "Deployment Context" section). Not a
/// `CatalogRecord`: the service catalog stores only the image and deployment identifier, not a
/// full runnable context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CronJobSpec {
    pub project: String,
    pub service: String,
    pub cron_name: String,
    /// Bumped by the CLI on every install; used with `canonical_hash` for `CronSpecApply`'s
    /// idempotent-upsert contract.
    pub revision: u64,
    /// Computed by `canonical_hash` over this spec's content fields (excludes identity/ownership
    /// bookkeeping so the CLI can compute it locally, without agent cooperation, purely to detect
    /// configuration drift -- see the plan's "Configuration Reconciliation" section).
    pub canonical_hash: String,
    /// Stamped by the receiving agent from its own local identity/membership, never trusted from
    /// the caller (mirrors `CatalogCommit`'s `owner_node_id`/`owner_epoch` handling in `api.rs`).
    pub owner_node_id: String,
    pub owner_epoch: u64,
    /// This node's own `MembershipRecord::server_name`, likewise agent-derived: the `jiji.server=`
    /// label a cron container carries has to match what a service container on this same host
    /// already carries (`container_runtime::render_labels`), and the agent's own membership
    /// record is the authoritative source for that name -- not the caller.
    pub server: String,
    pub source_deployment_id: String,
    pub source_replica_id: String,
    pub image: String,
    /// A standard 5-field cron expression; already validated by `jiji_config::validation` before
    /// this spec is ever built.
    pub schedule: String,
    pub timezone: String,
    pub timeout_seconds: u64,
    pub overlap: CronOverlap,
    pub missed_runs: CronMissedRuns,
    pub command: Vec<String>,
    pub env_file_path: String,
    pub mount_args: Vec<String>,
    pub resource_args: Vec<String>,
    pub bridge_network: String,
    pub dns_address: String,
}

/// The subset of `CronJobSpec` that defines "what should run and when": excludes `project`,
/// `service`, `cron_name` (the storage key, not content), `revision`/`canonical_hash` (the hash
/// output itself), `owner_node_id`/`owner_epoch`/`server` (agent-derived, not something the CLI can
/// compute standalone -- see `CronJobSpec::owner_epoch`'s doc comment). Kept as a separate type
/// rather than hashing `CronJobSpec` directly so adding a future bookkeeping field to the spec
/// never silently changes every installed job's hash.
#[derive(Serialize)]
struct CronSpecContent<'a> {
    image: &'a str,
    schedule: &'a str,
    timezone: &'a str,
    timeout_seconds: u64,
    overlap: CronOverlap,
    missed_runs: CronMissedRuns,
    command: &'a [String],
    env_file_path: &'a str,
    mount_args: &'a [String],
    resource_args: &'a [String],
    bridge_network: &'a str,
    dns_address: &'a str,
    source_deployment_id: &'a str,
    source_replica_id: &'a str,
}

impl CronJobSpec {
    /// A plain `String`, not `Result`: `CronSpecContent` is composed entirely of strings,
    /// numbers, and simple enums, none of which can fail to serialize to JSON.
    pub fn canonical_hash(&self) -> String {
        let content = CronSpecContent {
            image: &self.image,
            schedule: &self.schedule,
            timezone: &self.timezone,
            timeout_seconds: self.timeout_seconds,
            overlap: self.overlap,
            missed_runs: self.missed_runs,
            command: &self.command,
            env_file_path: &self.env_file_path,
            mount_args: &self.mount_args,
            resource_args: &self.resource_args,
            bridge_network: &self.bridge_network,
            dns_address: &self.dns_address,
            source_deployment_id: &self.source_deployment_id,
            source_replica_id: &self.source_replica_id,
        };
        content_hash(&content).expect("CronSpecContent always serializes")
    }
}

/// Outcome of `AgentStore::apply_cron_spec`'s idempotent upsert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CronSpecApplyOutcome {
    Installed(CronJobSpec),
    /// An existing spec for this `(service, cron_name)` had a different `canonical_hash` and/or
    /// `revision`; replaced.
    Updated(CronJobSpec),
    /// An existing spec already matched both `revision` and `canonical_hash`; left untouched.
    Unchanged(CronJobSpec),
}

impl CronSpecApplyOutcome {
    pub fn spec(&self) -> &CronJobSpec {
        match self {
            CronSpecApplyOutcome::Installed(spec)
            | CronSpecApplyOutcome::Updated(spec)
            | CronSpecApplyOutcome::Unchanged(spec) => spec,
        }
    }

    pub fn kind(&self) -> CronSpecApplyOutcomeKind {
        match self {
            CronSpecApplyOutcome::Installed(_) => CronSpecApplyOutcomeKind::Installed,
            CronSpecApplyOutcome::Updated(_) => CronSpecApplyOutcomeKind::Updated,
            CronSpecApplyOutcome::Unchanged(_) => CronSpecApplyOutcomeKind::Unchanged,
        }
    }
}

/// Wire-friendly projection of `CronSpecApplyOutcome`'s variant, without repeating the spec
/// itself: the agent API's `CronSpecApplied` response carries `spec` and `outcome` as separate
/// fields (internally-tagged `ResponseBody` variants must stay flat structs, see `api.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CronSpecApplyOutcomeKind {
    Installed,
    Updated,
    Unchanged,
}

/// Why a run exists: a scheduler tick (`Scheduled`, tied to one `scheduled_at` UTC second) or an
/// operator-requested `jiji service cron run` (`Manual`, tied to nothing but its own `run_id`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CronRunCause {
    Scheduled,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CronRunState {
    Claimed,
    Running,
    Succeeded,
    Failed,
    TimedOut,
    Skipped,
}

impl CronRunState {
    /// `overlap: forbid` (the only supported value in this release) blocks a new claim while any
    /// run in one of these states exists for the same job.
    pub fn is_active(self) -> bool {
        matches!(self, CronRunState::Claimed | CronRunState::Running)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CronRun {
    pub run_id: String,
    pub project: String,
    pub service: String,
    pub cron_name: String,
    pub cause: CronRunCause,
    /// `Some` only for `cause: Scheduled`; the UTC second this run was due. `None` for a manual
    /// run, which "does not change the next scheduled time" (see the plan's CLI Surface section).
    pub scheduled_at: Option<u64>,
    pub claimed_at: u64,
    pub started_at: Option<u64>,
    pub finished_at: Option<u64>,
    pub state: CronRunState,
    /// The rest of these fields populate once container execution exists (a later phase); `None`
    /// for the lifetime of a run under this phase's agent, which claims runs but does not yet
    /// start containers for them.
    pub deployment_id: Option<String>,
    pub container_name: Option<String>,
    pub address: Option<String>,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
}

/// Outcome of `AgentStore::claim_cron_run`'s transactional claim (see the plan's "Scheduler
/// Rules" section).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CronClaimOutcome {
    Claimed(CronRun),
    /// This exact `(service, cron_name, scheduled_at)` was already claimed before (a scheduler
    /// restart re-evaluating the same tick, or a retried request); the plan requires returning
    /// the existing run and starting no new one, never erroring.
    DuplicateScheduledClaim(CronRun),
    /// `overlap: forbid` refused this claim because a run for the same job is still active.
    OverlapForbidden {
        active_run_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CronSchedulerState {
    pub service: String,
    pub cron_name: String,
    pub last_evaluated_at: Option<u64>,
    pub next_due_at: Option<u64>,
    pub skipped_overlap_count: u64,
}

/// Filter for `AgentStore::cron_runs`; every field is an AND-combined, optional narrowing (an
/// absent field matches everything), mirroring the `CronRuns` agent-API request's own filter set.
#[derive(Debug, Clone, Default)]
pub struct CronRunFilter {
    pub service: Option<String>,
    pub cron_name: Option<String>,
    pub run_id: Option<String>,
    /// Only runs claimed at or after this UTC second.
    pub since: Option<u64>,
    pub limit: Option<u32>,
}

/// The agent API's `CronStatus` response row for one installed job (see the plan's `jiji service
/// cron status` section): scheduler bookkeeping plus a summary of the most recent run, assembled
/// by `api.rs` from `cron_specs`/`cron_scheduler_state`/`cron_runs`/`active_cron_run` rather than
/// stored as its own row (it has no independent durable existence beyond those primitives).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CronJobStatus {
    pub service: String,
    pub cron_name: String,
    pub last_scheduled_at: Option<u64>,
    pub last_started_at: Option<u64>,
    pub last_finished_at: Option<u64>,
    pub last_state: Option<CronRunState>,
    pub last_exit_code: Option<i32>,
    pub next_due_at: Option<u64>,
    pub active_run_id: Option<String>,
    pub skipped_overlap_count: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(source_deployment_id: &str) -> CronJobSpec {
        CronJobSpec {
            project: "demo".into(),
            service: "twitch".into(),
            cron_name: "sync-twitch".into(),
            revision: 1,
            canonical_hash: String::new(),
            owner_node_id: "node-a".into(),
            owner_epoch: 1,
            server: "node-a".into(),
            source_deployment_id: source_deployment_id.into(),
            source_replica_id: "replica-a".into(),
            image: "ghcr.io/example/twitch-sync:latest".into(),
            schedule: "7 */2 * * *".into(),
            timezone: "UTC".into(),
            timeout_seconds: 3600,
            overlap: CronOverlap::Forbid,
            missed_runs: CronMissedRuns::Skip,
            command: vec!["npm".into(), "run".into(), "sync:twitch".into()],
            env_file_path: "/var/lib/jiji/demo/env/twitch".into(),
            mount_args: vec![],
            resource_args: vec![],
            bridge_network: "jiji-demo".into(),
            dns_address: "100.64.0.5".into(),
        }
    }

    #[test]
    fn canonical_hash_is_stable_for_identical_content() {
        assert_eq!(
            spec("dep-a").canonical_hash(),
            spec("dep-a").canonical_hash()
        );
    }

    #[test]
    fn canonical_hash_ignores_identity_and_ownership_fields() {
        let mut other = spec("dep-a");
        other.project = "other-project".into();
        other.service = "other-service".into();
        other.cron_name = "other-cron".into();
        other.revision = 99;
        other.owner_node_id = "node-z".into();
        other.owner_epoch = 42;
        assert_eq!(spec("dep-a").canonical_hash(), other.canonical_hash());
    }

    #[test]
    fn canonical_hash_changes_with_content() {
        assert_ne!(
            spec("dep-a").canonical_hash(),
            spec("dep-b").canonical_hash()
        );
    }
}
