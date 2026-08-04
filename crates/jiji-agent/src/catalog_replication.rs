//! Bounded peer-to-peer catalog anti-entropy, mirroring `replication.rs`'s membership exchange
//! but over its own port (`jiji_network::catalog_replication_port`) and its own signed-operation
//! type. Kept as a separate module/wire type rather than folded into `replication.rs`, matching
//! this codebase's existing per-concern separation (`wireguard.rs`: "service/catalog changes never
//! enter this module").
//!
//! Verifying a received catalog operation requires the current membership view (a node's signature
//! is only valid while it holds an active membership record, see `catalog.rs`), so every exchange
//! rebuilds a `MembershipView` from the store's already-persisted, already-authenticated membership
//! operations -- catalog replication never re-authenticates membership itself, only reads it.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::catalog::{SignedCatalogOperation, CATALOG_PROTOCOL_VERSION, CATALOG_SCHEMA_VERSION};
use crate::desired::SignedDesiredState;
use crate::membership::{AuthorityKeyring, MembershipError, MembershipView};
use crate::store::{AgentStore, StoreError};

pub const MAX_CATALOG_FRAME_BYTES: u32 = 1024 * 1024;
pub const MAX_CATALOG_RECORDS: usize = 4096;
pub const MAX_DESIRED_RECORDS: usize = 1024;
/// Bounds the complete connect/write/read exchange so one wedged peer cannot freeze catalog
/// freshness (and therefore DNS eligibility) for the whole node.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Exchange {
    project_id: String,
    recovery_epoch: u64,
    protocol_version: u16,
    schema_version: u16,
    operations: Vec<SignedCatalogOperation>,
    #[serde(default)]
    desired_operations: Vec<SignedDesiredState>,
}

impl Exchange {
    fn from_store(
        store: &AgentStore,
        authority: &AuthorityKeyring,
    ) -> Result<Self, CatalogReplicationError> {
        let exchange = Self {
            project_id: authority.project_id().to_string(),
            recovery_epoch: authority.recovery_epoch(),
            protocol_version: CATALOG_PROTOCOL_VERSION,
            schema_version: CATALOG_SCHEMA_VERSION,
            operations: store.catalog_snapshot_operations()?,
            desired_operations: store.desired_snapshot_operations()?,
        };
        exchange.validate(authority)?;
        Ok(exchange)
    }

    fn validate(&self, authority: &AuthorityKeyring) -> Result<(), CatalogReplicationError> {
        if self.project_id != authority.project_id() {
            return Err(CatalogReplicationError::WrongProject);
        }
        if self.recovery_epoch != authority.recovery_epoch() {
            return Err(CatalogReplicationError::RecoveryEpoch);
        }
        if self.protocol_version != CATALOG_PROTOCOL_VERSION
            || self.schema_version != CATALOG_SCHEMA_VERSION
        {
            return Err(CatalogReplicationError::IncompatibleVersion);
        }
        if self.operations.len() > MAX_CATALOG_RECORDS {
            return Err(CatalogReplicationError::SnapshotTooLarge {
                kind: "catalog",
                records: self.operations.len(),
                limit: MAX_CATALOG_RECORDS,
            });
        }
        if self.desired_operations.len() > MAX_DESIRED_RECORDS {
            return Err(CatalogReplicationError::SnapshotTooLarge {
                kind: "desired-state",
                records: self.desired_operations.len(),
                limit: MAX_DESIRED_RECORDS,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum CatalogReplicationError {
    #[error("catalog replication i/o failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("catalog replication serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("catalog replication store failed: {0}")]
    Store(#[from] StoreError),
    #[error("catalog replication membership check failed: {0}")]
    Membership(#[from] MembershipError),
    #[error("catalog replication frame exceeds the configured limit")]
    FrameTooLarge,
    #[error("{kind} snapshot has {records} records, exceeding the supported limit of {limit}")]
    SnapshotTooLarge {
        kind: &'static str,
        records: usize,
        limit: usize,
    },
    #[error("catalog replication peer belongs to another project")]
    WrongProject,
    #[error("catalog replication peer belongs to another recovery epoch")]
    RecoveryEpoch,
    #[error("catalog replication peer uses an incompatible protocol or schema")]
    IncompatibleVersion,
    #[error("agent store lock is poisoned")]
    LockPoisoned,
    #[error("catalog replication exchange timed out")]
    Timeout,
}

pub async fn sync_once(
    address: std::net::SocketAddr,
    store: Arc<Mutex<AgentStore>>,
    authority: Arc<AuthorityKeyring>,
) -> Result<(), CatalogReplicationError> {
    sync_once_with_timeout(address, store, authority, EXCHANGE_TIMEOUT).await
}

async fn sync_once_with_timeout(
    address: std::net::SocketAddr,
    store: Arc<Mutex<AgentStore>>,
    authority: Arc<AuthorityKeyring>,
    timeout: Duration,
) -> Result<(), CatalogReplicationError> {
    let outbound = {
        let store = store
            .lock()
            .map_err(|_| CatalogReplicationError::LockPoisoned)?;
        Exchange::from_store(&store, &authority)?
    };
    tokio::time::timeout(timeout, async {
        let mut stream = TcpStream::connect(address).await?;
        write_exchange(&mut stream, &outbound).await?;
        let inbound = read_exchange(&mut stream).await?;
        apply_exchange(inbound, &store, &authority)
    })
    .await
    .map_err(|_| CatalogReplicationError::Timeout)?
}

pub async fn serve(
    listener: TcpListener,
    store: Arc<Mutex<AgentStore>>,
    authority: Arc<AuthorityKeyring>,
) -> Result<(), CatalogReplicationError> {
    loop {
        let (mut stream, _) = listener.accept().await?;
        let store = Arc::clone(&store);
        let authority = Arc::clone(&authority);
        tokio::spawn(async move {
            let result = tokio::time::timeout(EXCHANGE_TIMEOUT, async {
                let inbound = read_exchange(&mut stream).await?;
                apply_exchange(inbound, &store, &authority)?;
                let outbound = {
                    let store = store
                        .lock()
                        .map_err(|_| CatalogReplicationError::LockPoisoned)?;
                    Exchange::from_store(&store, &authority)?
                };
                write_exchange(&mut stream, &outbound).await
            })
            .await
            .map_err(|_| CatalogReplicationError::Timeout)
            .and_then(|result| result);
            if let Err(error) = result {
                tracing::warn!(%error, "catalog replication exchange rejected");
            }
        });
    }
}

fn apply_exchange(
    exchange: Exchange,
    store: &Arc<Mutex<AgentStore>>,
    authority: &AuthorityKeyring,
) -> Result<(), CatalogReplicationError> {
    exchange.validate(authority)?;
    let store = store
        .lock()
        .map_err(|_| CatalogReplicationError::LockPoisoned)?;
    let membership_ops = store.membership_operations()?;
    let membership = MembershipView::from_operations(membership_ops, authority)?;
    for operation in exchange.operations {
        store.apply_catalog(
            &operation,
            authority.project_id(),
            authority.recovery_epoch(),
            &membership,
        )?;
    }
    for operation in exchange.desired_operations {
        store.apply_desired(
            &operation,
            authority.project_id(),
            authority.recovery_epoch(),
            &membership,
        )?;
    }
    Ok(())
}

async fn write_exchange(
    stream: &mut TcpStream,
    exchange: &Exchange,
) -> Result<(), CatalogReplicationError> {
    let payload = serde_json::to_vec(exchange)?;
    if payload.len() > MAX_CATALOG_FRAME_BYTES as usize {
        return Err(CatalogReplicationError::FrameTooLarge);
    }
    stream
        .write_all(&(payload.len() as u32).to_be_bytes())
        .await?;
    stream.write_all(&payload).await?;
    stream.flush().await?;
    Ok(())
}

async fn read_exchange(stream: &mut TcpStream) -> Result<Exchange, CatalogReplicationError> {
    let mut length = [0; 4];
    stream.read_exact(&mut length).await?;
    let length = u32::from_be_bytes(length);
    if length > MAX_CATALOG_FRAME_BYTES {
        return Err(CatalogReplicationError::FrameTooLarge);
    }
    let mut payload = vec![0; length as usize];
    stream.read_exact(&mut payload).await?;
    Ok(serde_json::from_slice(&payload)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{CatalogRecord, DeploymentState, HealthState};
    use crate::membership::{
        MembershipRecord, MembershipState, SignedMembership, MEMBERSHIP_PROTOCOL_VERSION,
        MEMBERSHIP_SCHEMA_VERSION,
    };
    use ed25519_dalek::SigningKey;
    use tempfile::tempdir;

    fn fixture() -> (Arc<AuthorityKeyring>, SigningKey, SigningKey) {
        let authority_key = SigningKey::from_bytes(&[4; 32]);
        let node_key = SigningKey::from_bytes(&[6; 32]);
        let mut authority = AuthorityKeyring::new("project", 1);
        authority.add_authority("root", authority_key.verifying_key());
        (Arc::new(authority), authority_key, node_key)
    }

    #[tokio::test]
    async fn peer_that_accepts_without_reply_is_bounded_by_exchange_timeout() {
        let (authority, _, _) = fixture();
        let dir = tempdir().unwrap();
        let store = Arc::new(Mutex::new(
            AgentStore::open(&dir.path().join("agent.sqlite3")).unwrap(),
        ));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let stalled_peer = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(60)).await;
        });

        let result =
            sync_once_with_timeout(address, store, authority, Duration::from_millis(50)).await;

        assert!(matches!(result, Err(CatalogReplicationError::Timeout)));
        stalled_peer.abort();
    }

    fn membership_operation(authority_key: &SigningKey, node_key: &SigningKey) -> SignedMembership {
        SignedMembership::sign(
            MembershipRecord {
                project_id: "project".into(),
                recovery_epoch: 1,
                protocol_version: MEMBERSHIP_PROTOCOL_VERSION,
                schema_version: MEMBERSHIP_SCHEMA_VERSION,
                node_id: "node-a".into(),
                server_name: "node-a".into(),
                node_signing_public_key: node_key.verifying_key().to_bytes().to_vec(),
                wireguard_public_key: "wg-a".into(),
                management_address: "100.98.64.1".parse().unwrap(),
                container_subnet: "198.18.1.0/24".into(),
                endpoints: vec!["192.0.2.1:51820".parse().unwrap()],
                owner_epoch: 1,
                revision: 1,
                state: MembershipState::Active,
            },
            "root",
            authority_key,
        )
        .unwrap()
    }

    fn catalog_operation(node_key: &SigningKey) -> SignedCatalogOperation {
        SignedCatalogOperation::sign(
            CatalogRecord {
                project_id: "project".into(),
                recovery_epoch: 1,
                protocol_version: CATALOG_PROTOCOL_VERSION,
                schema_version: CATALOG_SCHEMA_VERSION,
                service: "web".into(),
                replica_id: "r1".into(),
                owner_node_id: "node-a".into(),
                owner_epoch: 1,
                revision: 1,
                deployment_id: "deploy-1".into(),
                address: "198.18.1.10".parse().unwrap(),
                ports: vec![80],
                image: "nginx:alpine".into(),
                state: DeploymentState::Active,
                health: HealthState::Healthy,
            },
            node_key,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn one_peer_write_reaches_another_without_cli_fanout() {
        let (authority, authority_key, node_key) = fixture();
        let left_dir = tempdir().unwrap();
        let right_dir = tempdir().unwrap();
        let left = Arc::new(Mutex::new(
            AgentStore::open(&left_dir.path().join("agent.sqlite3")).unwrap(),
        ));
        let right = Arc::new(Mutex::new(
            AgentStore::open(&right_dir.path().join("agent.sqlite3")).unwrap(),
        ));
        // Both sides must already know about node-a's membership to verify its catalog signature.
        for store in [&left, &right] {
            store
                .lock()
                .unwrap()
                .apply_membership(&membership_operation(&authority_key, &node_key), &authority)
                .unwrap();
        }
        left.lock()
            .unwrap()
            .apply_catalog(&catalog_operation(&node_key), "project", 1, &{
                let mut view = MembershipView::default();
                view.apply(membership_operation(&authority_key, &node_key), &authority)
                    .unwrap();
                view
            })
            .unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(serve(listener, Arc::clone(&right), Arc::clone(&authority)));
        sync_once(address, Arc::clone(&left), Arc::clone(&authority))
            .await
            .unwrap();
        assert_eq!(right.lock().unwrap().latest_catalog().unwrap().len(), 1);
        server.abort();
    }

    #[tokio::test]
    async fn wrong_project_exchange_is_rejected_before_applying_operations() {
        let (authority, _, _) = fixture();
        let dir = tempdir().unwrap();
        let store = Arc::new(Mutex::new(
            AgentStore::open(&dir.path().join("agent.sqlite3")).unwrap(),
        ));
        let exchange = Exchange {
            project_id: "other".into(),
            recovery_epoch: 1,
            protocol_version: CATALOG_PROTOCOL_VERSION,
            schema_version: CATALOG_SCHEMA_VERSION,
            operations: vec![],
            desired_operations: vec![],
        };
        assert!(matches!(
            apply_exchange(exchange, &store, &authority),
            Err(CatalogReplicationError::WrongProject)
        ));
    }

    /// Phase 8 exit criterion: "mixed-version nodes are rejected rather than partially joined."
    /// ADR 0002 requires the first release to support exactly protocol 1/schema 1 and reject
    /// every other version before exchanging state -- there is no mixed-version cluster.
    #[tokio::test]
    async fn mismatched_protocol_version_is_rejected_before_applying_operations() {
        let (authority, _, node_key) = fixture();
        let dir = tempdir().unwrap();
        let store = Arc::new(Mutex::new(
            AgentStore::open(&dir.path().join("agent.sqlite3")).unwrap(),
        ));
        let exchange = Exchange {
            project_id: "project".into(),
            recovery_epoch: 1,
            protocol_version: CATALOG_PROTOCOL_VERSION + 1,
            schema_version: CATALOG_SCHEMA_VERSION,
            operations: vec![catalog_operation(&node_key)],
            desired_operations: vec![],
        };
        assert!(matches!(
            apply_exchange(exchange, &store, &authority),
            Err(CatalogReplicationError::IncompatibleVersion)
        ));
        assert!(store.lock().unwrap().latest_catalog().unwrap().is_empty());
    }

    #[tokio::test]
    async fn mismatched_schema_version_is_rejected_before_applying_operations() {
        let (authority, _, node_key) = fixture();
        let dir = tempdir().unwrap();
        let store = Arc::new(Mutex::new(
            AgentStore::open(&dir.path().join("agent.sqlite3")).unwrap(),
        ));
        let exchange = Exchange {
            project_id: "project".into(),
            recovery_epoch: 1,
            protocol_version: CATALOG_PROTOCOL_VERSION,
            schema_version: CATALOG_SCHEMA_VERSION + 1,
            operations: vec![catalog_operation(&node_key)],
            desired_operations: vec![],
        };
        assert!(matches!(
            apply_exchange(exchange, &store, &authority),
            Err(CatalogReplicationError::IncompatibleVersion)
        ));
        assert!(store.lock().unwrap().latest_catalog().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_catalog_write_from_a_node_the_peer_has_no_membership_record_for_is_rejected() {
        let (authority, _authority_key, node_key) = fixture();
        let dir = tempdir().unwrap();
        // The receiving side never learned node-a's membership, so it cannot verify node-a's
        // catalog signature -- the write must not silently apply.
        let store = Arc::new(Mutex::new(
            AgentStore::open(&dir.path().join("agent.sqlite3")).unwrap(),
        ));
        let exchange = Exchange {
            project_id: "project".into(),
            recovery_epoch: 1,
            protocol_version: CATALOG_PROTOCOL_VERSION,
            schema_version: CATALOG_SCHEMA_VERSION,
            operations: vec![catalog_operation(&node_key)],
            desired_operations: vec![],
        };
        assert!(apply_exchange(exchange, &store, &authority).is_err());
        assert!(store.lock().unwrap().latest_catalog().unwrap().is_empty());
    }
}
