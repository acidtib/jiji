//! The agent's Unix-socket control API: length-prefixed JSON request/response framing over a
//! root-owned, `0600`-mode socket in a `0700` directory (physical access is already restricted by
//! the filesystem; `peer_cred` re-checks it in-process so a misconfigured socket mode fails
//! closed rather than open). Supports health, identity, diagnostics, reconciliation status,
//! catalog reads/listing, and a local-transaction primitive. `CatalogRead`/`LocalTransaction`
//! remain the generic Phase 2 key/value primitive (see `store.rs`'s `commit_local_transaction`);
//! `CatalogList` (Phase 4) is the real, node-signed, replicated service catalog from `catalog.rs`.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Semaphore;
use tracing::{debug, warn};

use crate::catalog::{
    CatalogRecord, DeploymentState, HealthState, SignedCatalogOperation, CATALOG_PROTOCOL_VERSION,
    CATALOG_SCHEMA_VERSION,
};
use crate::desired::{
    DesiredStateRecord, ReplicaAssignment, SignedDesiredState, DESIRED_PROTOCOL_VERSION,
    DESIRED_SCHEMA_VERSION,
};
use crate::leases::{AddressAllocator, DEFAULT_QUARANTINE_SECONDS};
use crate::membership::{AuthorityKeyring, MembershipView};
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
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
        replica_override: Option<u32>,
        assignments: Vec<ReplicaAssignment>,
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseBody {
    Health {
        schema_version: i64,
        observation_count: i64,
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
    catalog_authority: Option<CatalogAuthority>,
}

#[derive(Clone)]
pub struct CatalogAuthority {
    pub project_id: String,
    pub recovery_epoch: u64,
    pub node_id: String,
    pub node_key: ed25519_dalek::SigningKey,
    pub membership_authority: AuthorityKeyring,
}

impl AgentApi {
    pub fn new(store: Arc<Mutex<AgentStore>>, identity: Identity, socket_path: String) -> Self {
        Self {
            store,
            identity,
            socket_path,
            started_at: Instant::now(),
            peer_reachability_timeout_secs: 30,
            catalog_authority: None,
        }
    }

    pub fn with_catalog_authority(mut self, authority: CatalogAuthority) -> Self {
        self.catalog_authority = Some(authority);
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
                replica_override,
                assignments,
            } => {
                let authority = self.catalog_authority.as_ref().ok_or_else(|| {
                    ApiError::new(
                        ErrorCode::Invalid,
                        "desired-state authority is not configured",
                    )
                })?;
                let membership = MembershipView::from_operations(
                    store
                        .membership_operations()
                        .map_err(|error| internal(&error))?,
                    &authority.membership_authority,
                )
                .map_err(|error| ApiError::new(ErrorCode::Internal, error.to_string()))?;
                let author = membership.get(&authority.node_id).ok_or_else(|| {
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
                    project_id: authority.project_id.clone(),
                    recovery_epoch: authority.recovery_epoch,
                    protocol_version: DESIRED_PROTOCOL_VERSION,
                    schema_version: DESIRED_SCHEMA_VERSION,
                    service,
                    replica_override,
                    assignments,
                    revision,
                    author_node_id: authority.node_id.clone(),
                    author_epoch: author.record.owner_epoch,
                };
                let operation = SignedDesiredState::sign(record.clone(), &authority.node_key)
                    .map_err(|error| ApiError::new(ErrorCode::Invalid, error.to_string()))?;
                store
                    .apply_desired(
                        &operation,
                        &authority.project_id,
                        authority.recovery_epoch,
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
                let authority = self.catalog_authority.as_ref().ok_or_else(|| {
                    ApiError::new(ErrorCode::Invalid, "catalog authority is not configured")
                })?;
                let address = address.parse().map_err(|error: std::net::AddrParseError| {
                    ApiError::new(ErrorCode::Invalid, error.to_string())
                })?;
                let membership = MembershipView::from_operations(
                    store
                        .membership_operations()
                        .map_err(|error| internal(&error))?,
                    &authority.membership_authority,
                )
                .map_err(|error| ApiError::new(ErrorCode::Internal, error.to_string()))?;
                let owner = membership.get(&authority.node_id).ok_or_else(|| {
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
                        && record.owner_node_id != authority.node_id
                        && record.owner_epoch >= owner.record.owner_epoch
                }) {
                    return Err(ApiError::new(
                        ErrorCode::Invalid,
                        "replica is fenced to a different owner",
                    ));
                }
                let record = CatalogRecord {
                    project_id: authority.project_id.clone(),
                    recovery_epoch: authority.recovery_epoch,
                    protocol_version: CATALOG_PROTOCOL_VERSION,
                    schema_version: CATALOG_SCHEMA_VERSION,
                    service,
                    replica_id,
                    owner_node_id: authority.node_id.clone(),
                    owner_epoch: owner.record.owner_epoch,
                    revision: next_revision,
                    deployment_id,
                    address,
                    ports,
                    image,
                    state,
                    health,
                };
                let operation =
                    SignedCatalogOperation::sign(record.clone(), &authority.node_key)
                        .map_err(|error| ApiError::new(ErrorCode::Invalid, error.to_string()))?;
                store
                    .apply_catalog(
                        &operation,
                        &authority.project_id,
                        authority.recovery_epoch,
                        &membership,
                    )
                    .map_err(|error| internal(&error))?;
                Ok(ResponseBody::CatalogCommitted { record })
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
}
