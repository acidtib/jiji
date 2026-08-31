//! The agent's Unix-socket control API: length-prefixed JSON request/response framing over a
//! root-owned, `0600`-mode socket in a `0700` directory (physical access is already restricted by
//! the filesystem; `peer_cred` re-checks it in-process so a misconfigured socket mode fails
//! closed rather than open). Supports health, identity, diagnostics, reconciliation status,
//! catalog reads/listing, and a local-transaction primitive. `CatalogRead`/`LocalTransaction`
//! remain the generic Phase 2 key/value primitive (see `store.rs`'s `commit_local_transaction`);
//! `CatalogList` (Phase 4) is the real, node-owned, replicated service catalog from `catalog.rs`.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Semaphore;
use tracing::{debug, warn};

use crate::candidate_health::CandidateHealthCheckSpec;
use crate::catalog::{
    CatalogRecord, DeploymentState, HealthState, CATALOG_PROTOCOL_VERSION, CATALOG_SCHEMA_VERSION,
};
use crate::cron::{
    CronClaimOutcome, CronJobSpec, CronJobStatus, CronMissedRuns, CronOverlap, CronRun,
    CronRunCause, CronRunFilter, CronSpecApplyOutcomeKind,
};
use crate::desired::{DesiredStateRecord, DESIRED_PROTOCOL_VERSION, DESIRED_SCHEMA_VERSION};
use crate::image_retention::ImageRetentionSpec;
use crate::leases::{AddressAllocator, DEFAULT_QUARANTINE_SECONDS};
use crate::membership::{MembershipView, NodeIdentity, RecordProvenance};
use crate::store::{AgentStore, ComponentStatus, OperationCounts, PeerSyncStatus};

/// A declared frame length above this is rejected before the body is read, so an oversized
/// request never causes an unbounded allocation.
pub const MAX_REQUEST_BYTES: u32 = 1024 * 1024;
/// Bounds concurrent in-flight connections; beyond this, new connections are told `Busy`
/// immediately instead of queueing indefinitely (ADR 0001 / Phase 2 "backpressure" exit
/// criterion).
pub const MAX_CONCURRENT_CONNECTIONS: usize = 32;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Request {
    pub idempotency_key: Option<String>,
    #[serde(flatten)]
    pub body: RequestBody,
}

// One request/response value exists at a time per exchange (never batched into a bulk
// collection), so `CronSpecApply`/`CronSpecApplied` being much larger than e.g. `Health` costs a
// few hundred stack bytes per call, not a real allocation/throughput concern; boxing an arbitrary
// field to appease the size heuristic would be noise, not a fix.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum RequestBody {
    Health,
    Identity,
    Diagnostics,
    Compact,
    ReconciliationStatus,
    CatalogRead {
        key: String,
    },
    LocalTransaction {
        key: String,
        value: String,
    },
    /// The winning catalog record for every known replica this node has observed or replicated,
    /// including tombstones -- primarily a diagnostics/inspection surface for now; DNS answers
    /// come from `dns.rs`'s own zone build, not this call.
    CatalogList,
    AllocateAddress {
        deployment_id: String,
        replica_id: String,
        subnet: String,
        reserved: Vec<String>,
        timestamp: u64,
    },
    ReleaseAddress {
        deployment_id: String,
        timestamp: u64,
    },
    DesiredCommit {
        service: String,
        scale_override: Option<u32>,
    },
    DesiredRead {
        service: String,
    },
    CatalogCommit {
        service: String,
        replica_id: String,
        deployment_id: String,
        address: String,
        ports: Vec<u16>,
        image: String,
        state: DeploymentState,
        health: HealthState,
    },
    /// Idempotent upsert by `(service, cron_name, revision, canonical_hash)`; `owner_node_id`/
    /// `owner_epoch` are never taken from the caller (see `CronJobSpec`'s doc comment) --
    /// `jiji-cli` sends everything else, already rendered
    /// (`docs/architecture-notes.md#ownership-and-reconciliation`).
    #[allow(clippy::too_many_arguments)]
    CronSpecApply {
        service: String,
        cron_name: String,
        revision: u64,
        canonical_hash: String,
        source_deployment_id: String,
        source_replica_id: String,
        image: String,
        schedule: String,
        timezone: String,
        timeout_seconds: u64,
        overlap: CronOverlap,
        missed_runs: CronMissedRuns,
        command: Vec<String>,
        env_file_path: String,
        mount_args: Vec<String>,
        resource_args: Vec<String>,
        bridge_network: String,
        dns_address: String,
    },
    CronSpecRemove {
        service: String,
        cron_name: String,
    },
    /// Every cron spec installed on this agent (`list`'s per-host installation state, and the
    /// source of `list`'s canonical-hash drift comparison).
    CronSpecList,
    /// An absent `service`/`cron_name` matches every installed job with that field unconstrained,
    /// same as `CronRuns`' filter.
    CronStatus {
        service: Option<String>,
        cron_name: Option<String>,
    },
    /// Requests an immediate run of an already-installed job. `timestamp` is caller-supplied (not
    /// read from the wall clock here) so the claim, and any future scheduler-driven claim
    /// alongside it, stays testable against a controllable clock.
    CronRun {
        service: String,
        cron_name: String,
        timestamp: u64,
    },
    CronRuns {
        service: Option<String>,
        cron_name: Option<String>,
        run_id: Option<String>,
        since: Option<u64>,
        limit: Option<u32>,
    },
    /// Idempotent upsert by `(service, repo, retain)`; pushed identically to every host in a
    /// service's eligible `servers:` set after a successful deploy (see
    /// `image_retention_reconcile.rs` in `jiji-cli`), unlike `CronSpecApply` there is no single
    /// owner to derive here.
    ImageRetentionApply {
        service: String,
        repo: String,
        retain: u32,
    },
    ImageRetentionRemove {
        service: String,
    },
    /// Every image-retention spec installed on this agent.
    ImageRetentionList,
    /// Records a candidate's deploy-time health-check command for later replay (see
    /// `candidate_health.rs`). Idempotent upsert by `deployment_id`, local-only. An older agent
    /// that doesn't recognize this variant fails the request harmlessly; the caller treats it as
    /// best-effort.
    RecordCandidateHealthCheck {
        deployment_id: String,
        service: String,
        replica_id: String,
        command: String,
        interval_secs: u64,
        deploy_timeout_secs: u64,
    },
}

// See `RequestBody`'s doc comment on the identical `large_enum_variant` allow.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum ResponseBody {
    Health {
        schema_version: i64,
        observation_count: i64,
        version: String,
    },
    Identity {
        project: String,
        engine: String,
        pid: u32,
    },
    Diagnostics {
        schema_version: i64,
        observation_count: i64,
        socket_path: String,
        uptime_seconds: u64,
        database_usage_bytes: u64,
        database_soft_quota_bytes: Option<u64>,
        operation_counts: OperationCounts,
        peer_reachability_timeout_secs: u64,
        peer_sync: Vec<PeerSyncStatus>,
        components: Vec<ComponentStatus>,
    },
    Compacted {
        membership_removed: usize,
        catalog_removed: usize,
        desired_removed: usize,
    },
    ReconciliationStatus {
        last_discovery_at: Option<String>,
        observation_count: i64,
        peer_sync: Vec<PeerSyncStatus>,
        components: Vec<ComponentStatus>,
    },
    CatalogRead {
        value: Option<String>,
        revision: Option<i64>,
    },
    LocalTransaction {
        revision: i64,
    },
    CatalogList {
        records: Vec<CatalogRecord>,
    },
    AddressLease {
        deployment_id: String,
        replica_id: String,
        address: String,
        state: String,
    },
    AddressReleased {
        released: bool,
    },
    DesiredState {
        record: Option<DesiredStateRecord>,
    },
    CatalogCommitted {
        record: CatalogRecord,
    },
    CronSpecApplied {
        spec: CronJobSpec,
        outcome: CronSpecApplyOutcomeKind,
    },
    CronSpecRemoved {
        removed: bool,
    },
    CronSpecs {
        specs: Vec<CronJobSpec>,
    },
    CronStatuses {
        statuses: Vec<CronJobStatus>,
    },
    CronRunAccepted {
        run_id: String,
    },
    /// `overlap: forbid` refused the run; `active_run_id` names the run already occupying this
    /// job so the caller can inspect it instead (see the plan's `jiji service cron run` section).
    CronRunConflict {
        active_run_id: String,
    },
    CronRuns {
        runs: Vec<CronRun>,
    },
    ImageRetentionApplied {
        spec: ImageRetentionSpec,
    },
    ImageRetentionRemoved {
        removed: bool,
    },
    ImageRetentionSpecs {
        specs: Vec<ImageRetentionSpec>,
    },
    CandidateHealthCheckRecorded,
}

pub type ApiResult = Result<ResponseBody, ApiError>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
}

impl ApiError {
    fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// A stable contract the CLI can key retries off of: `Busy` is safe to retry, `Invalid` and
/// `RequestTooLarge` are caller bugs, `Unauthorized`/`Internal` are not retry-worthy without
/// operator action.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Unauthorized,
    RequestTooLarge,
    Busy,
    NotFound,
    Invalid,
    Internal,
}

#[derive(Clone)]
pub struct Identity {
    pub project: String,
    pub engine: String,
}

#[derive(Clone)]
pub struct AgentApi {
    store: Arc<Mutex<AgentStore>>,
    identity: Identity,
    socket_path: String,
    started_at: Instant,
    peer_reachability_timeout_secs: u64,
    catalog_identity: Option<NodeIdentity>,
    /// Needed only for `CronRun` (to actually start a container locally, unlike every other
    /// request here which is a pure store operation); `None` in tests that never exercise it.
    engine: Option<crate::engine::Engine>,
    /// Supplies `CronRun`'s address-allocation subnet/reserved set (mirroring exactly what
    /// `jiji-cli` passes for a service's own `AllocateAddress` call -- see
    /// `deploy_transaction.rs`'s `deploy_dynamic_endpoint`), since a cron run is claimed and
    /// executed by this agent on its own initiative, with no CLI round-trip to supply them.
    mesh_config: Option<Arc<crate::runtime::MeshConfig>>,
}

impl AgentApi {
    pub fn new(store: Arc<Mutex<AgentStore>>, identity: Identity, socket_path: String) -> Self {
        Self {
            store,
            identity,
            socket_path,
            started_at: Instant::now(),
            peer_reachability_timeout_secs: 30,
            catalog_identity: None,
            engine: None,
            mesh_config: None,
        }
    }

    pub fn with_engine(mut self, engine: crate::engine::Engine) -> Self {
        self.engine = Some(engine);
        self
    }

    pub fn with_mesh_config(mut self, mesh_config: Arc<crate::runtime::MeshConfig>) -> Self {
        self.mesh_config = Some(mesh_config);
        self
    }

    /// This node's own identity for catalog/desired-state writes it originates locally, via
    /// `CatalogCommit`/`DesiredCommit`. Trusted unconditionally (`RecordProvenance::Local`) -- see
    /// `catalog.rs`'s module doc comment.
    pub fn with_catalog_identity(mut self, identity: NodeIdentity) -> Self {
        self.catalog_identity = Some(identity);
        self
    }

    pub fn with_peer_reachability_timeout(mut self, timeout_secs: u64) -> Self {
        self.peer_reachability_timeout_secs = timeout_secs;
        self
    }

    fn handle(&self, body: RequestBody) -> ApiResult {
        let store = self
            .store
            .lock()
            .map_err(|_| ApiError::new(ErrorCode::Internal, "local store lock poisoned"))?;
        match body {
            RequestBody::Health => Ok(ResponseBody::Health {
                schema_version: store.schema_version().map_err(|error| internal(&error))?,
                observation_count: store
                    .observation_count()
                    .map_err(|error| internal(&error))?,
                version: env!("CARGO_PKG_VERSION").to_string(),
            }),
            RequestBody::Identity => Ok(ResponseBody::Identity {
                project: self.identity.project.clone(),
                engine: self.identity.engine.clone(),
                pid: std::process::id(),
            }),
            RequestBody::Diagnostics => Ok(ResponseBody::Diagnostics {
                schema_version: store.schema_version().map_err(|error| internal(&error))?,
                observation_count: store
                    .observation_count()
                    .map_err(|error| internal(&error))?,
                socket_path: self.socket_path.clone(),
                uptime_seconds: self.started_at.elapsed().as_secs(),
                database_usage_bytes: store
                    .database_usage_bytes()
                    .map_err(|error| internal(&error))?,
                database_soft_quota_bytes: store.soft_quota_bytes(),
                operation_counts: store.operation_counts().map_err(|error| internal(&error))?,
                peer_reachability_timeout_secs: self.peer_reachability_timeout_secs,
                peer_sync: store
                    .peer_sync_statuses()
                    .map_err(|error| internal(&error))?,
                components: store
                    .component_statuses()
                    .map_err(|error| internal(&error))?,
            }),
            RequestBody::Compact => {
                let result = store
                    .compact_operations()
                    .map_err(|error| internal(&error))?;
                Ok(ResponseBody::Compacted {
                    membership_removed: result.membership_removed,
                    catalog_removed: result.catalog_removed,
                    desired_removed: result.desired_removed,
                })
            }
            RequestBody::ReconciliationStatus => Ok(ResponseBody::ReconciliationStatus {
                last_discovery_at: store
                    .get_checkpoint("last_discovery_at")
                    .map_err(|error| internal(&error))?,
                observation_count: store
                    .observation_count()
                    .map_err(|error| internal(&error))?,
                peer_sync: store
                    .peer_sync_statuses()
                    .map_err(|error| internal(&error))?,
                components: store
                    .component_statuses()
                    .map_err(|error| internal(&error))?,
            }),
            RequestBody::CatalogRead { key } => {
                let entry = store
                    .read_local_state(&key)
                    .map_err(|error| internal(&error))?;
                Ok(ResponseBody::CatalogRead {
                    value: entry.as_ref().map(|(value, _)| value.clone()),
                    revision: entry.map(|(_, revision)| revision),
                })
            }
            RequestBody::LocalTransaction { key, value } => {
                let revision = store
                    .commit_local_transaction(&key, &value)
                    .map_err(|error| internal(&error))?;
                Ok(ResponseBody::LocalTransaction { revision })
            }
            RequestBody::CatalogList => Ok(ResponseBody::CatalogList {
                records: store.latest_catalog().map_err(|error| internal(&error))?,
            }),
            RequestBody::AllocateAddress {
                deployment_id,
                replica_id,
                subnet,
                reserved,
                timestamp,
            } => {
                let subnet = subnet
                    .parse()
                    .map_err(|error: jiji_network::NetworkPlanError| {
                        ApiError::new(ErrorCode::Invalid, error.to_string())
                    })?;
                let reserved = reserved
                    .iter()
                    .map(|value| value.parse())
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|error: std::net::AddrParseError| {
                        ApiError::new(ErrorCode::Invalid, error.to_string())
                    })?;
                let lease = AddressAllocator::new(&store, subnet, reserved)
                    .allocate(&deployment_id, &replica_id, timestamp)
                    .map_err(|error| ApiError::new(ErrorCode::Invalid, error.to_string()))?;
                Ok(ResponseBody::AddressLease {
                    deployment_id: lease.deployment_id,
                    replica_id: lease.replica_id,
                    address: lease.address.to_string(),
                    state: lease.state,
                })
            }
            RequestBody::ReleaseAddress {
                deployment_id,
                timestamp,
            } => {
                let released = store
                    .quarantine_address_lease(
                        &deployment_id,
                        timestamp.saturating_add(DEFAULT_QUARANTINE_SECONDS),
                    )
                    .map_err(|error| internal(&error))?;
                Ok(ResponseBody::AddressReleased { released })
            }
            RequestBody::DesiredRead { service } => {
                let record = store
                    .desired_state(&service)
                    .map_err(|error| internal(&error))?;
                Ok(ResponseBody::DesiredState { record })
            }
            RequestBody::DesiredCommit {
                service,
                scale_override,
            } => {
                let identity = self.catalog_identity.as_ref().ok_or_else(|| {
                    ApiError::new(
                        ErrorCode::Invalid,
                        "desired-state identity is not configured",
                    )
                })?;
                let scope = identity.scope();
                let membership = MembershipView::from_records(
                    store
                        .membership_operations()
                        .map_err(|error| internal(&error))?,
                    &scope,
                )
                .map_err(|error| ApiError::new(ErrorCode::Internal, error.to_string()))?;
                let author = membership.get(&identity.node_id).ok_or_else(|| {
                    ApiError::new(
                        ErrorCode::Invalid,
                        "local node has no active membership record",
                    )
                })?;
                let revision = store
                    .desired_state(&service)
                    .map_err(|error| internal(&error))?
                    .map_or(1, |current| current.revision + 1);
                let record = DesiredStateRecord {
                    project_id: identity.project_id.clone(),
                    recovery_epoch: identity.recovery_epoch,
                    protocol_version: DESIRED_PROTOCOL_VERSION,
                    schema_version: DESIRED_SCHEMA_VERSION,
                    service,
                    scale_override,
                    revision,
                    author_node_id: identity.node_id.clone(),
                    author_epoch: author.owner_epoch,
                };
                store
                    .apply_desired(
                        record.clone(),
                        RecordProvenance::Local,
                        &identity.project_id,
                        identity.recovery_epoch,
                        &membership,
                    )
                    .map_err(|error| internal(&error))?;
                Ok(ResponseBody::DesiredState {
                    record: Some(record),
                })
            }
            RequestBody::CatalogCommit {
                service,
                replica_id,
                deployment_id,
                address,
                ports,
                image,
                state,
                health,
            } => {
                let identity = self.catalog_identity.as_ref().ok_or_else(|| {
                    ApiError::new(ErrorCode::Invalid, "catalog identity is not configured")
                })?;
                let address = address.parse().map_err(|error: std::net::AddrParseError| {
                    ApiError::new(ErrorCode::Invalid, error.to_string())
                })?;
                let scope = identity.scope();
                let membership = MembershipView::from_records(
                    store
                        .membership_operations()
                        .map_err(|error| internal(&error))?,
                    &scope,
                )
                .map_err(|error| ApiError::new(ErrorCode::Internal, error.to_string()))?;
                let owner = membership.get(&identity.node_id).ok_or_else(|| {
                    ApiError::new(
                        ErrorCode::Invalid,
                        "local node has no active membership record",
                    )
                })?;
                let catalog = store.latest_catalog().map_err(|error| internal(&error))?;
                let next_revision = catalog
                    .iter()
                    .filter(|record| record.replica_id == replica_id)
                    .map(|record| record.revision)
                    .max()
                    .unwrap_or(0)
                    .saturating_add(1);
                if catalog.iter().any(|record| {
                    record.replica_id == replica_id
                        && record.owner_node_id != identity.node_id
                        && record.owner_epoch >= owner.owner_epoch
                }) {
                    return Err(ApiError::new(
                        ErrorCode::Invalid,
                        "replica is fenced to a different owner",
                    ));
                }
                let record = CatalogRecord {
                    project_id: identity.project_id.clone(),
                    recovery_epoch: identity.recovery_epoch,
                    protocol_version: CATALOG_PROTOCOL_VERSION,
                    schema_version: CATALOG_SCHEMA_VERSION,
                    service,
                    replica_id,
                    owner_node_id: identity.node_id.clone(),
                    owner_epoch: owner.owner_epoch,
                    revision: next_revision,
                    deployment_id,
                    address,
                    ports,
                    image,
                    state,
                    health,
                };
                store
                    .apply_catalog(
                        record.clone(),
                        RecordProvenance::Local,
                        &identity.project_id,
                        identity.recovery_epoch,
                        &membership,
                    )
                    .map_err(|error| internal(&error))?;
                Ok(ResponseBody::CatalogCommitted { record })
            }
            RequestBody::CronSpecApply {
                service,
                cron_name,
                revision,
                canonical_hash,
                source_deployment_id,
                source_replica_id,
                image,
                schedule,
                timezone,
                timeout_seconds,
                overlap,
                missed_runs,
                command,
                env_file_path,
                mount_args,
                resource_args,
                bridge_network,
                dns_address,
            } => {
                let identity = self.catalog_identity.as_ref().ok_or_else(|| {
                    ApiError::new(ErrorCode::Invalid, "cron identity is not configured")
                })?;
                let scope = identity.scope();
                let membership = MembershipView::from_records(
                    store
                        .membership_operations()
                        .map_err(|error| internal(&error))?,
                    &scope,
                )
                .map_err(|error| ApiError::new(ErrorCode::Internal, error.to_string()))?;
                let owner = membership.get(&identity.node_id).ok_or_else(|| {
                    ApiError::new(
                        ErrorCode::Invalid,
                        "local node has no active membership record",
                    )
                })?;
                let spec = CronJobSpec {
                    project: identity.project_id.clone(),
                    service,
                    cron_name,
                    revision,
                    canonical_hash,
                    owner_node_id: identity.node_id.clone(),
                    owner_epoch: owner.owner_epoch,
                    server: owner.server_name.clone(),
                    source_deployment_id,
                    source_replica_id,
                    image,
                    schedule,
                    timezone,
                    timeout_seconds,
                    overlap,
                    missed_runs,
                    command,
                    env_file_path,
                    mount_args,
                    resource_args,
                    bridge_network,
                    dns_address,
                };
                let outcome = store
                    .apply_cron_spec(&spec)
                    .map_err(|error| internal(&error))?;
                Ok(ResponseBody::CronSpecApplied {
                    outcome: outcome.kind(),
                    spec: outcome.spec().clone(),
                })
            }
            RequestBody::CronSpecRemove { service, cron_name } => {
                let removed = store
                    .remove_cron_spec(&service, &cron_name)
                    .map_err(|error| internal(&error))?;
                Ok(ResponseBody::CronSpecRemoved { removed })
            }
            RequestBody::CronSpecList => Ok(ResponseBody::CronSpecs {
                specs: store.cron_specs().map_err(|error| internal(&error))?,
            }),
            RequestBody::CronStatus { service, cron_name } => {
                let specs = store.cron_specs().map_err(|error| internal(&error))?;
                let mut statuses = Vec::new();
                for spec in specs
                    .into_iter()
                    .filter(|spec| service.as_deref().is_none_or(|s| s == spec.service))
                    .filter(|spec| cron_name.as_deref().is_none_or(|c| c == spec.cron_name))
                {
                    let scheduler_state = store
                        .cron_scheduler_state(&spec.service, &spec.cron_name)
                        .map_err(|error| internal(&error))?;
                    let last_run = store
                        .cron_runs(&CronRunFilter {
                            service: Some(spec.service.clone()),
                            cron_name: Some(spec.cron_name.clone()),
                            limit: Some(1),
                            ..Default::default()
                        })
                        .map_err(|error| internal(&error))?
                        .into_iter()
                        .next();
                    let active_run = store
                        .active_cron_run(&spec.service, &spec.cron_name)
                        .map_err(|error| internal(&error))?;
                    statuses.push(CronJobStatus {
                        service: spec.service,
                        cron_name: spec.cron_name,
                        last_scheduled_at: last_run.as_ref().and_then(|run| run.scheduled_at),
                        last_started_at: last_run.as_ref().and_then(|run| run.started_at),
                        last_finished_at: last_run.as_ref().and_then(|run| run.finished_at),
                        last_state: last_run.as_ref().map(|run| run.state),
                        last_exit_code: last_run.as_ref().and_then(|run| run.exit_code),
                        next_due_at: scheduler_state.as_ref().and_then(|s| s.next_due_at),
                        active_run_id: active_run.map(|run| run.run_id),
                        skipped_overlap_count: scheduler_state
                            .map_or(0, |s| s.skipped_overlap_count),
                    });
                }
                Ok(ResponseBody::CronStatuses { statuses })
            }
            RequestBody::CronRun {
                service,
                cron_name,
                timestamp,
            } => {
                let Some(spec) = store
                    .cron_spec(&service, &cron_name)
                    .map_err(|error| internal(&error))?
                else {
                    return Err(ApiError::new(
                        ErrorCode::NotFound,
                        format!(
                            "service '{service}' has no installed cron job named '{cron_name}'"
                        ),
                    ));
                };
                let run_id = generate_cron_run_id(&self.identity.project, &service, &cron_name);
                let outcome = store
                    .claim_cron_run(
                        &self.identity.project,
                        &service,
                        &cron_name,
                        CronRunCause::Manual,
                        None,
                        &run_id,
                        timestamp,
                    )
                    .map_err(|error| internal(&error))?;
                let run = match outcome {
                    CronClaimOutcome::Claimed(run) => run,
                    CronClaimOutcome::OverlapForbidden { active_run_id } => {
                        return Ok(ResponseBody::CronRunConflict { active_run_id });
                    }
                    CronClaimOutcome::DuplicateScheduledClaim(_) => {
                        return Err(ApiError::new(
                            ErrorCode::Internal,
                            "a manual run unexpectedly produced a scheduled-claim outcome",
                        ));
                    }
                };
                let (Some(engine), Some(mesh_config)) = (self.engine, self.mesh_config.as_ref())
                else {
                    let _ = store.finish_cron_run(
                        &run.run_id,
                        crate::cron::CronRunState::Failed,
                        timestamp,
                        None,
                        Some(
                            "agent has no engine/mesh runtime configured; cannot execute cron containers"
                                .to_string(),
                        ),
                    );
                    return Err(ApiError::new(
                        ErrorCode::Internal,
                        "agent has no engine/mesh runtime configured; cannot execute cron containers",
                    ));
                };
                let run_id = run.run_id.clone();
                crate::cron_exec::lease_and_spawn(
                    &store,
                    Arc::clone(&self.store),
                    engine,
                    mesh_config,
                    spec,
                    run,
                    timestamp,
                )
                .map_err(|error| ApiError::new(ErrorCode::Internal, error))?;
                Ok(ResponseBody::CronRunAccepted { run_id })
            }
            RequestBody::CronRuns {
                service,
                cron_name,
                run_id,
                since,
                limit,
            } => {
                let runs = store
                    .cron_runs(&CronRunFilter {
                        service,
                        cron_name,
                        run_id,
                        since,
                        limit,
                    })
                    .map_err(|error| internal(&error))?;
                Ok(ResponseBody::CronRuns { runs })
            }
            RequestBody::ImageRetentionApply {
                service,
                repo,
                retain,
            } => {
                let spec = ImageRetentionSpec {
                    service,
                    repo,
                    retain,
                };
                let stored = store
                    .apply_image_retention_spec(&spec)
                    .map_err(|error| internal(&error))?;
                Ok(ResponseBody::ImageRetentionApplied { spec: stored })
            }
            RequestBody::ImageRetentionRemove { service } => {
                let removed = store
                    .remove_image_retention_spec(&service)
                    .map_err(|error| internal(&error))?;
                Ok(ResponseBody::ImageRetentionRemoved { removed })
            }
            RequestBody::ImageRetentionList => Ok(ResponseBody::ImageRetentionSpecs {
                specs: store
                    .image_retention_specs()
                    .map_err(|error| internal(&error))?,
            }),
            RequestBody::RecordCandidateHealthCheck {
                deployment_id,
                service,
                replica_id,
                command,
                interval_secs,
                deploy_timeout_secs,
            } => {
                store
                    .record_candidate_health_check(&CandidateHealthCheckSpec {
                        deployment_id,
                        service,
                        replica_id,
                        command,
                        interval_secs,
                        deploy_timeout_secs,
                    })
                    .map_err(|error| internal(&error))?;
                Ok(ResponseBody::CandidateHealthCheckRecorded)
            }
        }
    }

    fn handle_idempotent(&self, request: Request) -> ApiResult {
        let Some(key) = request.idempotency_key.as_deref() else {
            return self.handle(request.body);
        };
        let cached = {
            let store = self
                .store
                .lock()
                .map_err(|_| ApiError::new(ErrorCode::Internal, "local store lock poisoned"))?;
            store
                .idempotent_get(key)
                .map_err(|error| internal(&error))?
        };
        if let Some(cached) = cached {
            return match serde_json::from_str::<ApiResult>(&cached) {
                Ok(result) => result,
                Err(error) => Err(ApiError::new(ErrorCode::Internal, error.to_string())),
            };
        }
        let result = self.handle(request.body);
        let store = self
            .store
            .lock()
            .map_err(|_| ApiError::new(ErrorCode::Internal, "local store lock poisoned"))?;
        if let Ok(serialized) = serde_json::to_string(&result) {
            let _ = store.idempotent_put(key, &serialized);
        }
        result
    }
}

fn internal(error: &crate::store::StoreError) -> ApiError {
    ApiError::new(ErrorCode::Internal, error.to_string())
}

/// Mirrors `jiji-cli`'s `deploy_transaction.rs` deployment-id generation: a SHA-256 hex digest of
/// a wall-clock-nanosecond-plus-pid nonce, not a `uuid` dependency this crate otherwise has no
/// need for.
fn generate_cron_run_id(project: &str, service: &str, cron_name: &str) -> String {
    use sha2::{Digest, Sha256};
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Sha256::digest(
        format!(
            "{project}\0{service}\0{cron_name}\0{nonce}\0{}",
            std::process::id()
        )
        .as_bytes(),
    )
    .iter()
    .map(|byte| format!("{byte:02x}"))
    .collect()
}

/// Accepts connections until `listener` is dropped, handling backpressure by immediately
/// rejecting connections beyond `MAX_CONCURRENT_CONNECTIONS` instead of queueing them.
pub async fn serve(listener: UnixListener, api: AgentApi) {
    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_CONNECTIONS));
    loop {
        let (stream, _addr) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(error) => {
                warn!(%error, "agent API accept failed");
                continue;
            }
        };
        match Arc::clone(&semaphore).try_acquire_owned() {
            Ok(permit) => {
                let api = api.clone();
                tokio::spawn(async move {
                    handle_connection(stream, api).await;
                    drop(permit);
                });
            }
            Err(_) => {
                tokio::spawn(async move {
                    let mut stream = stream;
                    let _ = write_frame(
                        &mut stream,
                        &Err::<ResponseBody, _>(ApiError::new(
                            ErrorCode::Busy,
                            "agent is at its concurrent-connection limit; retry shortly",
                        )),
                    )
                    .await;
                });
            }
        }
    }
}

async fn handle_connection(mut stream: UnixStream, api: AgentApi) {
    if !is_authorized_peer(&stream) {
        let _ = write_frame(
            &mut stream,
            &Err::<ResponseBody, _>(ApiError::new(
                ErrorCode::Unauthorized,
                "connecting peer is not authorized to use this socket",
            )),
        )
        .await;
        return;
    }

    loop {
        let request = match read_request(&mut stream).await {
            Ok(Some(request)) => request,
            Ok(None) => return,
            Err(error) => {
                let _ = write_frame(&mut stream, &Err::<ResponseBody, _>(error)).await;
                return;
            }
        };
        let result = api.handle_idempotent(request);
        if write_frame(&mut stream, &result).await.is_err() {
            return;
        }
    }
}

/// `Some(uid == 0 || uid == this process's own uid)` is deliberately conservative: the socket is
/// already root-owned mode `0600` in a `0700` directory, so this is defense in depth, not the
/// only gate.
fn is_authorized_peer(stream: &UnixStream) -> bool {
    match stream.peer_cred() {
        Ok(credentials) => is_authorized_uid(credentials.uid()),
        Err(error) => {
            debug!(%error, "could not read peer credentials; refusing connection");
            false
        }
    }
}

fn is_authorized_uid(uid: u32) -> bool {
    uid == 0 || uid == current_uid()
}

fn current_uid() -> u32 {
    // Safety: `getuid` takes no arguments and cannot fail.
    unsafe { libc::getuid() }
}

async fn read_request(stream: &mut UnixStream) -> Result<Option<Request>, ApiError> {
    let mut length_bytes = [0u8; 4];
    match stream.read_exact(&mut length_bytes).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(_) => return Ok(None),
    }
    let length = u32::from_be_bytes(length_bytes);
    if length > MAX_REQUEST_BYTES {
        // Deliberately does not attempt to read/discard the declared body: the sender is over
        // the limit regardless of what follows, and this connection is about to be closed.
        return Err(ApiError::new(
            ErrorCode::RequestTooLarge,
            format!("request of {length} bytes exceeds the {MAX_REQUEST_BYTES}-byte limit"),
        ));
    }
    let mut body = vec![0u8; length as usize];
    stream
        .read_exact(&mut body)
        .await
        .map_err(|error| ApiError::new(ErrorCode::Invalid, error.to_string()))?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|error| ApiError::new(ErrorCode::Invalid, error.to_string()))
}

async fn write_frame(stream: &mut UnixStream, result: &ApiResult) -> std::io::Result<()> {
    let payload = serde_json::to_vec(result).unwrap_or_else(|_| {
        serde_json::to_vec(&Err::<ResponseBody, _>(ApiError::new(
            ErrorCode::Internal,
            "response serialization failed",
        )))
        .expect("fallback error response always serializes")
    });
    stream
        .write_all(&(payload.len() as u32).to_be_bytes())
        .await?;
    stream.write_all(&payload).await?;
    stream.flush().await
}

/// A single request/response exchange used by the `jiji-agent ping` smoke-test subcommand and by
/// tests; opens a fresh connection per call.
pub async fn call(socket_path: &std::path::Path, request: &Request) -> std::io::Result<ApiResult> {
    let mut stream = UnixStream::connect(socket_path).await?;
    let payload = serde_json::to_vec(request)?;
    stream
        .write_all(&(payload.len() as u32).to_be_bytes())
        .await?;
    stream.write_all(&payload).await?;
    stream.flush().await?;

    let mut length_bytes = [0u8; 4];
    stream.read_exact(&mut length_bytes).await?;
    let length = u32::from_be_bytes(length_bytes) as usize;
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body).await?;
    serde_json::from_slice(&body)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn test_api(dir: &std::path::Path) -> AgentApi {
        let store = AgentStore::open(&dir.join("agent.sqlite3")).unwrap();
        AgentApi::new(
            Arc::new(Mutex::new(store)),
            Identity {
                project: "demo".into(),
                engine: "docker".into(),
            },
            dir.join("agent.sock").display().to_string(),
        )
    }

    async fn spawn_server(dir: &std::path::Path) -> std::path::PathBuf {
        let socket_path = dir.join("agent.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let api = test_api(dir);
        tokio::spawn(serve(listener, api));
        // Give the accept loop a chance to start; connecting before bind-and-listen races only
        // matters in this synthetic single-process test, not the real systemd-managed socket.
        tokio::task::yield_now().await;
        socket_path
    }

    #[tokio::test]
    async fn health_and_identity_round_trip() {
        let dir = tempdir().unwrap();
        let socket_path = spawn_server(dir.path()).await;

        let response = call(
            &socket_path,
            &Request {
                idempotency_key: None,
                body: RequestBody::Identity,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            response,
            Ok(ResponseBody::Identity {
                project: "demo".into(),
                engine: "docker".into(),
                pid: std::process::id(),
            })
        );

        let response = call(
            &socket_path,
            &Request {
                idempotency_key: None,
                body: RequestBody::Health,
            },
        )
        .await
        .unwrap();
        assert!(matches!(response, Ok(ResponseBody::Health { .. })));
    }

    #[tokio::test]
    async fn duplicate_idempotency_key_does_not_reapply_the_transaction() {
        let dir = tempdir().unwrap();
        let socket_path = spawn_server(dir.path()).await;
        let request = Request {
            idempotency_key: Some("req-1".into()),
            body: RequestBody::LocalTransaction {
                key: "counter".into(),
                value: "v1".into(),
            },
        };

        let first = call(&socket_path, &request).await.unwrap();
        let second = call(&socket_path, &request).await.unwrap();
        assert_eq!(first, second);
        assert_eq!(first, Ok(ResponseBody::LocalTransaction { revision: 1 }));

        // A request with no idempotency key is not deduplicated: it applies again.
        let third = call(
            &socket_path,
            &Request {
                idempotency_key: None,
                body: RequestBody::LocalTransaction {
                    key: "counter".into(),
                    value: "v2".into(),
                },
            },
        )
        .await
        .unwrap();
        assert_eq!(third, Ok(ResponseBody::LocalTransaction { revision: 2 }));
    }

    #[tokio::test]
    async fn oversized_request_is_rejected_without_hanging() {
        let dir = tempdir().unwrap();
        let socket_path = spawn_server(dir.path()).await;

        let mut stream = UnixStream::connect(&socket_path).await.unwrap();
        stream
            .write_all(&(MAX_REQUEST_BYTES + 1).to_be_bytes())
            .await
            .unwrap();
        // Deliberately never sends the (huge, undeclared) body -- a well-behaved server rejects
        // based on the declared length alone.

        let mut length_bytes = [0u8; 4];
        stream.read_exact(&mut length_bytes).await.unwrap();
        let length = u32::from_be_bytes(length_bytes) as usize;
        let mut body = vec![0u8; length];
        stream.read_exact(&mut body).await.unwrap();
        let response: ApiResult = serde_json::from_slice(&body).unwrap();
        assert_eq!(response.unwrap_err().code, ErrorCode::RequestTooLarge);
    }

    #[tokio::test]
    async fn connections_beyond_the_limit_get_a_busy_response_instead_of_hanging() {
        let dir = tempdir().unwrap();
        let socket_path = spawn_server(dir.path()).await;

        // Each held-open connection consumes one permit (it never sends a request, so its
        // `handle_connection` task stays parked in `read_request` holding the permit) until the
        // server is saturated, at which point the accept loop must still respond immediately
        // rather than queueing the next connection indefinitely.
        let mut holds = Vec::new();
        for _ in 0..MAX_CONCURRENT_CONNECTIONS {
            holds.push(UnixStream::connect(&socket_path).await.unwrap());
        }
        let extra = call(
            &socket_path,
            &Request {
                idempotency_key: None,
                body: RequestBody::Health,
            },
        )
        .await
        .unwrap();
        assert_eq!(extra.unwrap_err().code, ErrorCode::Busy);
        drop(holds);
    }

    #[tokio::test]
    async fn catalog_list_starts_empty_and_reflects_the_store() {
        let dir = tempdir().unwrap();
        let socket_path = spawn_server(dir.path()).await;
        let response = call(
            &socket_path,
            &Request {
                idempotency_key: None,
                body: RequestBody::CatalogList,
            },
        )
        .await
        .unwrap();
        assert_eq!(response, Ok(ResponseBody::CatalogList { records: vec![] }));
    }

    #[test]
    fn authorization_allows_only_root_and_this_process() {
        assert!(is_authorized_uid(0));
        assert!(is_authorized_uid(current_uid()));
        assert!(!is_authorized_uid(current_uid() + 12345));
    }

    fn seeded_cron_spec() -> CronJobSpec {
        let mut spec = CronJobSpec {
            project: "demo".into(),
            service: "twitch".into(),
            cron_name: "sync-twitch".into(),
            revision: 1,
            canonical_hash: String::new(),
            owner_node_id: "node-a".into(),
            owner_epoch: 1,
            server: "node-a".into(),
            source_deployment_id: "dep-a".into(),
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
        };
        spec.canonical_hash = spec.canonical_hash();
        spec
    }

    fn cron_spec_apply_request(revision: u64, canonical_hash: &str, schedule: &str) -> RequestBody {
        RequestBody::CronSpecApply {
            service: "twitch".into(),
            cron_name: "sync-twitch".into(),
            revision,
            canonical_hash: canonical_hash.into(),
            source_deployment_id: "dep-a".into(),
            source_replica_id: "replica-a".into(),
            image: "ghcr.io/example/twitch-sync:latest".into(),
            schedule: schedule.into(),
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

    /// Installs an `Active` membership record for `node-a` and returns the matching
    /// `NodeIdentity`, so `CronSpecApply`'s owner-derivation path (mirroring `CatalogCommit`'s)
    /// has a real local node to resolve.
    fn seed_membership(store: &AgentStore) -> NodeIdentity {
        use crate::membership::{
            MembershipRecord, MembershipScope, MembershipState, MEMBERSHIP_PROTOCOL_VERSION,
            MEMBERSHIP_SCHEMA_VERSION,
        };
        let scope = MembershipScope::new("demo", 1);
        let record = MembershipRecord {
            project_id: "demo".into(),
            recovery_epoch: 1,
            protocol_version: MEMBERSHIP_PROTOCOL_VERSION,
            schema_version: MEMBERSHIP_SCHEMA_VERSION,
            node_id: "node-a".into(),
            server_name: "node-a".into(),
            wireguard_public_key: "wg-node-a".into(),
            management_address: "100.98.64.2".parse().unwrap(),
            container_subnet: "198.18.2.0/24".into(),
            endpoints: vec!["192.0.2.2:51820".parse().unwrap()],
            owner_epoch: 7,
            revision: 1,
            state: MembershipState::Active,
        };
        store.apply_membership(record, &scope).unwrap();
        NodeIdentity {
            project_id: "demo".into(),
            recovery_epoch: 1,
            node_id: "node-a".into(),
        }
    }

    async fn spawn_server_with_catalog_identity(dir: &std::path::Path) -> std::path::PathBuf {
        let socket_path = dir.join("agent.sock");
        let store = AgentStore::open(&dir.join("agent.sqlite3")).unwrap();
        let identity = seed_membership(&store);
        let listener = UnixListener::bind(&socket_path).unwrap();
        let api = AgentApi::new(
            Arc::new(Mutex::new(store)),
            Identity {
                project: "demo".into(),
                engine: "docker".into(),
            },
            socket_path.display().to_string(),
        )
        .with_catalog_identity(identity);
        tokio::spawn(serve(listener, api));
        tokio::task::yield_now().await;
        socket_path
    }

    async fn spawn_server_with_seeded_store(
        dir: &std::path::Path,
        seed: impl FnOnce(&AgentStore),
    ) -> std::path::PathBuf {
        {
            let store = AgentStore::open(&dir.join("agent.sqlite3")).unwrap();
            seed(&store);
        }
        spawn_server(dir).await
    }

    #[tokio::test]
    async fn cron_spec_apply_derives_owner_from_membership_and_is_idempotent() {
        let dir = tempdir().unwrap();
        let socket_path = spawn_server_with_catalog_identity(dir.path()).await;

        let response = call(
            &socket_path,
            &Request {
                idempotency_key: None,
                body: cron_spec_apply_request(1, "hash-a", "7 */2 * * *"),
            },
        )
        .await
        .unwrap()
        .unwrap();
        let ResponseBody::CronSpecApplied { spec, outcome } = response else {
            panic!("expected CronSpecApplied");
        };
        assert_eq!(outcome, CronSpecApplyOutcomeKind::Installed);
        assert_eq!(spec.owner_node_id, "node-a");
        assert_eq!(spec.owner_epoch, 7);
        assert_eq!(spec.project, "demo");

        // Re-applying the identical spec is unchanged.
        let response = call(
            &socket_path,
            &Request {
                idempotency_key: None,
                body: cron_spec_apply_request(1, "hash-a", "7 */2 * * *"),
            },
        )
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(
            response,
            ResponseBody::CronSpecApplied {
                outcome: CronSpecApplyOutcomeKind::Unchanged,
                ..
            }
        ));

        // A different revision/hash updates.
        let response = call(
            &socket_path,
            &Request {
                idempotency_key: None,
                body: cron_spec_apply_request(2, "hash-b", "0 3 * * *"),
            },
        )
        .await
        .unwrap()
        .unwrap();
        let ResponseBody::CronSpecApplied { spec, outcome } = response else {
            panic!("expected CronSpecApplied");
        };
        assert_eq!(outcome, CronSpecApplyOutcomeKind::Updated);
        assert_eq!(spec.schedule, "0 3 * * *");

        let list = call(
            &socket_path,
            &Request {
                idempotency_key: None,
                body: RequestBody::CronSpecList,
            },
        )
        .await
        .unwrap()
        .unwrap();
        let ResponseBody::CronSpecs { specs } = list else {
            panic!("expected CronSpecs");
        };
        assert_eq!(specs.len(), 1);

        let removed = call(
            &socket_path,
            &Request {
                idempotency_key: None,
                body: RequestBody::CronSpecRemove {
                    service: "twitch".into(),
                    cron_name: "sync-twitch".into(),
                },
            },
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(removed, ResponseBody::CronSpecRemoved { removed: true });
    }

    #[tokio::test]
    async fn cron_spec_apply_without_configured_identity_is_rejected() {
        let dir = tempdir().unwrap();
        let socket_path = spawn_server(dir.path()).await;
        let response = call(
            &socket_path,
            &Request {
                idempotency_key: None,
                body: cron_spec_apply_request(1, "hash-a", "7 */2 * * *"),
            },
        )
        .await
        .unwrap();
        assert_eq!(response.unwrap_err().code, ErrorCode::Invalid);
    }

    #[tokio::test]
    async fn image_retention_apply_is_idempotent_and_list_remove_round_trip() {
        let dir = tempdir().unwrap();
        let socket_path = spawn_server(dir.path()).await;

        let apply_request = |repo: &str, retain: u32| RequestBody::ImageRetentionApply {
            service: "web".into(),
            repo: repo.into(),
            retain,
        };

        let response = call(
            &socket_path,
            &Request {
                idempotency_key: None,
                body: apply_request("ghcr.io/example/demo-web", 3),
            },
        )
        .await
        .unwrap()
        .unwrap();
        let ResponseBody::ImageRetentionApplied { spec } = response else {
            panic!("expected ImageRetentionApplied");
        };
        assert_eq!(spec.service, "web");
        assert_eq!(spec.repo, "ghcr.io/example/demo-web");
        assert_eq!(spec.retain, 3);

        // Re-applying the identical spec still reports the same content back.
        let response = call(
            &socket_path,
            &Request {
                idempotency_key: None,
                body: apply_request("ghcr.io/example/demo-web", 3),
            },
        )
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(
            response,
            ResponseBody::ImageRetentionApplied { .. }
        ));

        let list = call(
            &socket_path,
            &Request {
                idempotency_key: None,
                body: RequestBody::ImageRetentionList,
            },
        )
        .await
        .unwrap()
        .unwrap();
        let ResponseBody::ImageRetentionSpecs { specs } = list else {
            panic!("expected ImageRetentionSpecs");
        };
        assert_eq!(specs.len(), 1);

        let removed = call(
            &socket_path,
            &Request {
                idempotency_key: None,
                body: RequestBody::ImageRetentionRemove {
                    service: "web".into(),
                },
            },
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            removed,
            ResponseBody::ImageRetentionRemoved { removed: true }
        );
    }

    #[tokio::test]
    async fn cron_run_reports_conflict_while_a_run_is_already_active() {
        let dir = tempdir().unwrap();
        // Seeded directly through the store (not via a real `CronRun` call), so this test never
        // needs `.with_engine`/`.with_mesh_config` configured -- see the "not configured" test
        // below for that path, and `cron_exec.rs`'s own tests for actual execution behavior.
        let active_run_id = "run-already-active".to_string();
        let socket_path = spawn_server_with_seeded_store(dir.path(), |store| {
            store.apply_cron_spec(&seeded_cron_spec()).unwrap();
            let outcome = store
                .claim_cron_run(
                    "demo",
                    "twitch",
                    "sync-twitch",
                    crate::cron::CronRunCause::Manual,
                    None,
                    "run-already-active",
                    100,
                )
                .unwrap();
            assert!(matches!(outcome, crate::cron::CronClaimOutcome::Claimed(_)));
        })
        .await;

        let response = call(
            &socket_path,
            &Request {
                idempotency_key: None,
                body: RequestBody::CronRun {
                    service: "twitch".into(),
                    cron_name: "sync-twitch".into(),
                    timestamp: 101,
                },
            },
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(response, ResponseBody::CronRunConflict { active_run_id });
    }

    #[tokio::test]
    async fn cron_run_fails_cleanly_without_engine_and_mesh_config() {
        let dir = tempdir().unwrap();
        let socket_path = spawn_server_with_seeded_store(dir.path(), |store| {
            store.apply_cron_spec(&seeded_cron_spec()).unwrap();
        })
        .await;

        let response = call(
            &socket_path,
            &Request {
                idempotency_key: None,
                body: RequestBody::CronRun {
                    service: "twitch".into(),
                    cron_name: "sync-twitch".into(),
                    timestamp: 100,
                },
            },
        )
        .await
        .unwrap();
        assert_eq!(response.unwrap_err().code, ErrorCode::Internal);

        // The claim must not linger as a permanently "active" ghost run blocking every future
        // attempt: the handler finishes it as Failed before returning the error.
        let statuses = call(
            &socket_path,
            &Request {
                idempotency_key: None,
                body: RequestBody::CronStatus {
                    service: None,
                    cron_name: None,
                },
            },
        )
        .await
        .unwrap()
        .unwrap();
        let ResponseBody::CronStatuses { statuses } = statuses else {
            panic!("expected CronStatuses");
        };
        assert_eq!(statuses[0].active_run_id, None);
    }

    #[tokio::test]
    async fn cron_run_rejects_a_job_with_no_installed_spec() {
        let dir = tempdir().unwrap();
        let socket_path = spawn_server(dir.path()).await;
        let response = call(
            &socket_path,
            &Request {
                idempotency_key: None,
                body: RequestBody::CronRun {
                    service: "twitch".into(),
                    cron_name: "sync-twitch".into(),
                    timestamp: 100,
                },
            },
        )
        .await
        .unwrap();
        assert_eq!(response.unwrap_err().code, ErrorCode::NotFound);
    }

    #[tokio::test]
    async fn cron_runs_and_cron_status_reflect_a_claimed_run() {
        let dir = tempdir().unwrap();
        // Seeded directly through the store, same reasoning as
        // `cron_run_reports_conflict_while_a_run_is_already_active` above.
        let run_id = "run-1".to_string();
        let socket_path = spawn_server_with_seeded_store(dir.path(), |store| {
            store.apply_cron_spec(&seeded_cron_spec()).unwrap();
            let outcome = store
                .claim_cron_run(
                    "demo",
                    "twitch",
                    "sync-twitch",
                    crate::cron::CronRunCause::Manual,
                    None,
                    "run-1",
                    100,
                )
                .unwrap();
            assert!(matches!(outcome, crate::cron::CronClaimOutcome::Claimed(_)));
        })
        .await;

        let runs = call(
            &socket_path,
            &Request {
                idempotency_key: None,
                body: RequestBody::CronRuns {
                    service: Some("twitch".into()),
                    cron_name: None,
                    run_id: None,
                    since: None,
                    limit: None,
                },
            },
        )
        .await
        .unwrap()
        .unwrap();
        let ResponseBody::CronRuns { runs } = runs else {
            panic!("expected CronRuns");
        };
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run_id, run_id);

        let status = call(
            &socket_path,
            &Request {
                idempotency_key: None,
                body: RequestBody::CronStatus {
                    service: None,
                    cron_name: None,
                },
            },
        )
        .await
        .unwrap()
        .unwrap();
        let ResponseBody::CronStatuses { statuses } = status else {
            panic!("expected CronStatuses");
        };
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].active_run_id, Some(run_id));
        assert_eq!(statuses[0].skipped_overlap_count, 0);
    }
}
