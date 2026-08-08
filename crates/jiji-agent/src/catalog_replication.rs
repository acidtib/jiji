//! Direct, mesh-only catalog/desired-state anti-entropy over its own port
//! (`jiji_network::catalog_replication_port`).
//!
//! Unlike the pre-Phase-N design, a node's outbound exchange contains only
//! records it owns (`owner_node_id`/`author_node_id` equal to this node's own
//! `node_id`) -- there is no relay of a third node's records. That, combined
//! with every project being a full WireGuard mesh (every node reaches every
//! other node directly), is what makes a signature unnecessary: a receiving
//! node authenticates an inbound record by resolving the TCP connection's
//! actual source address against its local membership view
//! (`MembershipView::find_by_management_address`) and checking it matches
//! the record's claimed owner. WireGuard's own peer authentication makes that
//! source address unspoofable within the mesh, and because nothing is ever
//! relayed, that one hop of transport authentication is sufficient -- see
//! `catalog.rs`'s and `membership.rs`'s module doc comments for the full
//! model. The outbound side binds its socket to its own management address
//! explicitly (rather than trusting default route selection) so the peer
//! always observes exactly the address its own membership record claims.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket, TcpStream};

use crate::catalog::{CatalogRecord, CATALOG_PROTOCOL_VERSION, CATALOG_SCHEMA_VERSION};
use crate::desired::DesiredStateRecord;
use crate::membership::{MembershipError, MembershipView, NodeIdentity, RecordProvenance};
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
    operations: Vec<CatalogRecord>,
    #[serde(default)]
    desired_operations: Vec<DesiredStateRecord>,
}

impl Exchange {
    fn from_store(
        store: &AgentStore,
        identity: &NodeIdentity,
    ) -> Result<Self, CatalogReplicationError> {
        let operations = store
            .latest_catalog()?
            .into_iter()
            .filter(|record| record.owner_node_id == identity.node_id)
            .collect();
        let desired_operations = store
            .desired_snapshot_operations()?
            .into_iter()
            .filter(|record| record.author_node_id == identity.node_id)
            .collect();
        let exchange = Self {
            project_id: identity.project_id.clone(),
            recovery_epoch: identity.recovery_epoch,
            protocol_version: CATALOG_PROTOCOL_VERSION,
            schema_version: CATALOG_SCHEMA_VERSION,
            operations,
            desired_operations,
        };
        exchange.validate(identity)?;
        Ok(exchange)
    }

    fn validate(&self, identity: &NodeIdentity) -> Result<(), CatalogReplicationError> {
        if self.project_id != identity.project_id {
            return Err(CatalogReplicationError::WrongProject);
        }
        if self.recovery_epoch != identity.recovery_epoch {
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
    address: SocketAddr,
    local_address: Ipv4Addr,
    store: Arc<Mutex<AgentStore>>,
    identity: Arc<NodeIdentity>,
) -> Result<(), CatalogReplicationError> {
    sync_once_with_timeout(address, local_address, store, identity, EXCHANGE_TIMEOUT).await
}

async fn sync_once_with_timeout(
    address: SocketAddr,
    local_address: Ipv4Addr,
    store: Arc<Mutex<AgentStore>>,
    identity: Arc<NodeIdentity>,
    timeout: Duration,
) -> Result<(), CatalogReplicationError> {
    let outbound = {
        let store = store
            .lock()
            .map_err(|_| CatalogReplicationError::LockPoisoned)?;
        Exchange::from_store(&store, &identity)?
    };
    tokio::time::timeout(timeout, async {
        let socket = TcpSocket::new_v4()?;
        socket.bind(SocketAddr::new(IpAddr::V4(local_address), 0))?;
        let mut stream = socket.connect(address).await?;
        write_exchange(&mut stream, &outbound).await?;
        let inbound = read_exchange(&mut stream).await?;
        apply_exchange(inbound, &store, &identity, address)
    })
    .await
    .map_err(|_| CatalogReplicationError::Timeout)?
}

pub async fn serve(
    listener: TcpListener,
    store: Arc<Mutex<AgentStore>>,
    identity: Arc<NodeIdentity>,
) -> Result<(), CatalogReplicationError> {
    loop {
        let (mut stream, peer) = listener.accept().await?;
        let store = Arc::clone(&store);
        let identity = Arc::clone(&identity);
        tokio::spawn(async move {
            let result = tokio::time::timeout(EXCHANGE_TIMEOUT, async {
                let inbound = read_exchange(&mut stream).await?;
                apply_exchange(inbound, &store, &identity, peer)?;
                let outbound = {
                    let store = store
                        .lock()
                        .map_err(|_| CatalogReplicationError::LockPoisoned)?;
                    Exchange::from_store(&store, &identity)?
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
    identity: &NodeIdentity,
    peer: SocketAddr,
) -> Result<(), CatalogReplicationError> {
    exchange.validate(identity)?;
    let store = store
        .lock()
        .map_err(|_| CatalogReplicationError::LockPoisoned)?;
    let scope = identity.scope();
    let membership = MembershipView::from_records(store.membership_operations()?, &scope)?;
    let provenance = RecordProvenance::Peer(peer);
    for record in exchange.operations {
        store.apply_catalog(
            record,
            provenance,
            &identity.project_id,
            identity.recovery_epoch,
            &membership,
        )?;
    }
    for record in exchange.desired_operations {
        store.apply_desired(
            record,
            provenance,
            &identity.project_id,
            identity.recovery_epoch,
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
    use crate::catalog::{DeploymentState, HealthState};
    use crate::membership::{
        MembershipRecord, MembershipScope, MembershipState, MEMBERSHIP_PROTOCOL_VERSION,
        MEMBERSHIP_SCHEMA_VERSION,
    };
    use tempfile::tempdir;

    fn identity(node_id: &str) -> Arc<NodeIdentity> {
        Arc::new(NodeIdentity {
            project_id: "project".into(),
            recovery_epoch: 1,
            node_id: node_id.into(),
        })
    }

    fn membership_record(node_id: &str, address: Ipv4Addr, subnet_octet: u8) -> MembershipRecord {
        MembershipRecord {
            project_id: "project".into(),
            recovery_epoch: 1,
            protocol_version: MEMBERSHIP_PROTOCOL_VERSION,
            schema_version: MEMBERSHIP_SCHEMA_VERSION,
            node_id: node_id.into(),
            server_name: node_id.into(),
            wireguard_public_key: format!("wg-{node_id}"),
            management_address: address,
            container_subnet: format!("198.18.{subnet_octet}.0/24"),
            endpoints: vec!["192.0.2.1:51820".parse().unwrap()],
            owner_epoch: 1,
            revision: 1,
            state: MembershipState::Active,
        }
    }

    fn catalog_record(owner_node_id: &str) -> CatalogRecord {
        CatalogRecord {
            project_id: "project".into(),
            recovery_epoch: 1,
            protocol_version: CATALOG_PROTOCOL_VERSION,
            schema_version: CATALOG_SCHEMA_VERSION,
            service: "web".into(),
            replica_id: "r1".into(),
            owner_node_id: owner_node_id.into(),
            owner_epoch: 1,
            revision: 1,
            deployment_id: "deploy-1".into(),
            address: "198.18.1.10".parse().unwrap(),
            ports: vec![80],
            image: "nginx:alpine".into(),
            state: DeploymentState::Active,
            health: HealthState::Healthy,
        }
    }

    #[tokio::test]
    async fn peer_that_accepts_without_reply_is_bounded_by_exchange_timeout() {
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

        let result = sync_once_with_timeout(
            address,
            Ipv4Addr::LOCALHOST,
            store,
            identity("node-a"),
            Duration::from_millis(50),
        )
        .await;

        assert!(matches!(result, Err(CatalogReplicationError::Timeout)));
        stalled_peer.abort();
    }

    #[tokio::test]
    async fn one_peer_write_reaches_another_directly_without_cli_fanout() {
        // node-a's management address is 127.0.0.2 -- distinct from node-b's 127.0.0.1 -- so the
        // outbound socket's explicit bind lets node-b's server observe exactly that source address
        // and attribute the record to node-a.
        let left_dir = tempdir().unwrap();
        let right_dir = tempdir().unwrap();
        let left_store = Arc::new(Mutex::new(
            AgentStore::open(&left_dir.path().join("agent.sqlite3")).unwrap(),
        ));
        let right_store = Arc::new(Mutex::new(
            AgentStore::open(&right_dir.path().join("agent.sqlite3")).unwrap(),
        ));
        let scope = MembershipScope::new("project", 1);
        for store in [&left_store, &right_store] {
            let store = store.lock().unwrap();
            store
                .apply_membership(
                    membership_record("node-a", Ipv4Addr::new(127, 0, 0, 2), 1),
                    &scope,
                )
                .unwrap();
            store
                .apply_membership(
                    membership_record("node-b", Ipv4Addr::new(127, 0, 0, 1), 2),
                    &scope,
                )
                .unwrap();
        }
        {
            let store = left_store.lock().unwrap();
            let mut membership = MembershipView::default();
            membership
                .apply(
                    membership_record("node-a", Ipv4Addr::new(127, 0, 0, 2), 1),
                    &scope,
                )
                .unwrap();
            store
                .apply_catalog(
                    catalog_record("node-a"),
                    RecordProvenance::Local,
                    "project",
                    1,
                    &membership,
                )
                .unwrap();
        }

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(serve(
            listener,
            Arc::clone(&right_store),
            identity("node-b"),
        ));
        sync_once(
            address,
            Ipv4Addr::new(127, 0, 0, 2),
            Arc::clone(&left_store),
            identity("node-a"),
        )
        .await
        .unwrap();
        assert_eq!(
            right_store.lock().unwrap().latest_catalog().unwrap().len(),
            1
        );
        server.abort();
    }

    #[tokio::test]
    async fn wrong_project_exchange_is_rejected_before_applying_operations() {
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
            apply_exchange(
                exchange,
                &store,
                &identity("node-b"),
                "127.0.0.1:0".parse().unwrap()
            ),
            Err(CatalogReplicationError::WrongProject)
        ));
    }

    /// Phase 8 exit criterion: "mixed-version nodes are rejected rather than partially joined."
    /// ADR 0002 requires the first release to support exactly protocol 1/schema 1 and reject
    /// every other version before exchanging state -- there is no mixed-version cluster.
    #[tokio::test]
    async fn mismatched_protocol_version_is_rejected_before_applying_operations() {
        let dir = tempdir().unwrap();
        let store = Arc::new(Mutex::new(
            AgentStore::open(&dir.path().join("agent.sqlite3")).unwrap(),
        ));
        let exchange = Exchange {
            project_id: "project".into(),
            recovery_epoch: 1,
            protocol_version: CATALOG_PROTOCOL_VERSION + 1,
            schema_version: CATALOG_SCHEMA_VERSION,
            operations: vec![catalog_record("node-a")],
            desired_operations: vec![],
        };
        assert!(matches!(
            apply_exchange(
                exchange,
                &store,
                &identity("node-b"),
                "127.0.0.1:0".parse().unwrap()
            ),
            Err(CatalogReplicationError::IncompatibleVersion)
        ));
        assert!(store.lock().unwrap().latest_catalog().unwrap().is_empty());
    }

    #[tokio::test]
    async fn mismatched_schema_version_is_rejected_before_applying_operations() {
        let dir = tempdir().unwrap();
        let store = Arc::new(Mutex::new(
            AgentStore::open(&dir.path().join("agent.sqlite3")).unwrap(),
        ));
        let exchange = Exchange {
            project_id: "project".into(),
            recovery_epoch: 1,
            protocol_version: CATALOG_PROTOCOL_VERSION,
            schema_version: CATALOG_SCHEMA_VERSION + 1,
            operations: vec![catalog_record("node-a")],
            desired_operations: vec![],
        };
        assert!(matches!(
            apply_exchange(
                exchange,
                &store,
                &identity("node-b"),
                "127.0.0.1:0".parse().unwrap()
            ),
            Err(CatalogReplicationError::IncompatibleVersion)
        ));
        assert!(store.lock().unwrap().latest_catalog().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_catalog_write_from_an_address_with_no_membership_record_is_rejected() {
        let dir = tempdir().unwrap();
        // The receiving side never learned any membership for the connection's source address, so
        // it cannot attribute the record to anyone -- the write must not silently apply.
        let store = Arc::new(Mutex::new(
            AgentStore::open(&dir.path().join("agent.sqlite3")).unwrap(),
        ));
        let exchange = Exchange {
            project_id: "project".into(),
            recovery_epoch: 1,
            protocol_version: CATALOG_PROTOCOL_VERSION,
            schema_version: CATALOG_SCHEMA_VERSION,
            operations: vec![catalog_record("node-a")],
            desired_operations: vec![],
        };
        assert!(apply_exchange(
            exchange,
            &store,
            &identity("node-b"),
            "127.0.0.9:0".parse().unwrap()
        )
        .is_err());
        assert!(store.lock().unwrap().latest_catalog().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_catalog_write_claiming_a_different_owner_than_the_sending_address_is_rejected() {
        let dir = tempdir().unwrap();
        let store = Arc::new(Mutex::new(
            AgentStore::open(&dir.path().join("agent.sqlite3")).unwrap(),
        ));
        let scope = MembershipScope::new("project", 1);
        {
            let store = store.lock().unwrap();
            store
                .apply_membership(
                    membership_record("node-a", Ipv4Addr::new(127, 0, 0, 2), 1),
                    &scope,
                )
                .unwrap();
            store
                .apply_membership(
                    membership_record("node-c", Ipv4Addr::new(127, 0, 0, 3), 3),
                    &scope,
                )
                .unwrap();
        }
        // The connection's source address (127.0.0.2, node-a's own) tries to vouch for a record
        // claiming ownership by node-c instead.
        let exchange = Exchange {
            project_id: "project".into(),
            recovery_epoch: 1,
            protocol_version: CATALOG_PROTOCOL_VERSION,
            schema_version: CATALOG_SCHEMA_VERSION,
            operations: vec![catalog_record("node-c")],
            desired_operations: vec![],
        };
        assert!(apply_exchange(
            exchange,
            &store,
            &identity("node-b"),
            "127.0.0.2:0".parse().unwrap()
        )
        .is_err());
        assert!(store.lock().unwrap().latest_catalog().unwrap().is_empty());
    }
}
