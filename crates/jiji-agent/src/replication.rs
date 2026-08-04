//! Bounded peer-to-peer membership anti-entropy.
//!
//! Transport is intentionally not trusted: every received operation is
//! independently checked against the project authority before it reaches the
//! durable store. The production listener is bound to a WireGuard management
//! address; the same code can use a public bootstrap connection before the
//! first tunnel exists.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::membership::{
    AuthorityKeyring, SignedMembership, MEMBERSHIP_PROTOCOL_VERSION, MEMBERSHIP_SCHEMA_VERSION,
};
use crate::store::{AgentStore, StoreError};

pub const MAX_REPLICATION_FRAME_BYTES: u32 = 1024 * 1024;
pub const MAX_MEMBERSHIP_RECORDS: usize = 1024;
/// Bounds the complete connect/write/read exchange. A peer that accepts a TCP connection and
/// then stops making progress must never stall the runtime's reconciliation loop indefinitely.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Exchange {
    project_id: String,
    recovery_epoch: u64,
    protocol_version: u16,
    schema_version: u16,
    operations: Vec<SignedMembership>,
}

impl Exchange {
    fn from_store(
        store: &AgentStore,
        authority: &AuthorityKeyring,
    ) -> Result<Self, ReplicationError> {
        let exchange = Self {
            project_id: authority.project_id().to_string(),
            recovery_epoch: authority.recovery_epoch(),
            protocol_version: MEMBERSHIP_PROTOCOL_VERSION,
            schema_version: MEMBERSHIP_SCHEMA_VERSION,
            operations: store.membership_snapshot_operations()?,
        };
        exchange.validate(authority)?;
        Ok(exchange)
    }

    fn validate(&self, authority: &AuthorityKeyring) -> Result<(), ReplicationError> {
        if self.project_id != authority.project_id() {
            return Err(ReplicationError::WrongProject);
        }
        if self.recovery_epoch != authority.recovery_epoch() {
            return Err(ReplicationError::RecoveryEpoch);
        }
        if self.protocol_version != MEMBERSHIP_PROTOCOL_VERSION
            || self.schema_version != MEMBERSHIP_SCHEMA_VERSION
        {
            return Err(ReplicationError::IncompatibleVersion);
        }
        if self.operations.len() > MAX_MEMBERSHIP_RECORDS {
            return Err(ReplicationError::SnapshotTooLarge {
                records: self.operations.len(),
                limit: MAX_MEMBERSHIP_RECORDS,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum ReplicationError {
    #[error("replication i/o failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("replication serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("replication store failed: {0}")]
    Store(#[from] StoreError),
    #[error("replication frame exceeds the configured limit")]
    FrameTooLarge,
    #[error("membership snapshot has {records} records, exceeding the supported limit of {limit}")]
    SnapshotTooLarge { records: usize, limit: usize },
    #[error("replication peer belongs to another project")]
    WrongProject,
    #[error("replication peer belongs to another recovery epoch")]
    RecoveryEpoch,
    #[error("replication peer uses an incompatible protocol or schema")]
    IncompatibleVersion,
    #[error("agent store lock is poisoned")]
    LockPoisoned,
    #[error("membership replication exchange timed out")]
    Timeout,
}

pub async fn sync_once(
    address: std::net::SocketAddr,
    store: Arc<Mutex<AgentStore>>,
    authority: Arc<AuthorityKeyring>,
) -> Result<(), ReplicationError> {
    sync_once_with_timeout(address, store, authority, EXCHANGE_TIMEOUT).await
}

async fn sync_once_with_timeout(
    address: std::net::SocketAddr,
    store: Arc<Mutex<AgentStore>>,
    authority: Arc<AuthorityKeyring>,
    timeout: Duration,
) -> Result<(), ReplicationError> {
    let outbound = {
        let store = store.lock().map_err(|_| ReplicationError::LockPoisoned)?;
        Exchange::from_store(&store, &authority)?
    };
    tokio::time::timeout(timeout, async {
        let mut stream = TcpStream::connect(address).await?;
        write_exchange(&mut stream, &outbound).await?;
        let inbound = read_exchange(&mut stream).await?;
        apply_exchange(inbound, &store, &authority)
    })
    .await
    .map_err(|_| ReplicationError::Timeout)?
}

pub async fn serve(
    listener: TcpListener,
    store: Arc<Mutex<AgentStore>>,
    authority: Arc<AuthorityKeyring>,
) -> Result<(), ReplicationError> {
    loop {
        let (mut stream, _) = listener.accept().await?;
        let store = Arc::clone(&store);
        let authority = Arc::clone(&authority);
        tokio::spawn(async move {
            let result = tokio::time::timeout(EXCHANGE_TIMEOUT, async {
                let inbound = read_exchange(&mut stream).await?;
                apply_exchange(inbound, &store, &authority)?;
                let outbound = {
                    let store = store.lock().map_err(|_| ReplicationError::LockPoisoned)?;
                    Exchange::from_store(&store, &authority)?
                };
                write_exchange(&mut stream, &outbound).await
            })
            .await
            .map_err(|_| ReplicationError::Timeout)
            .and_then(|result| result);
            if let Err(error) = result {
                tracing::warn!(%error, "membership replication exchange rejected");
            }
        });
    }
}

fn apply_exchange(
    exchange: Exchange,
    store: &Arc<Mutex<AgentStore>>,
    authority: &AuthorityKeyring,
) -> Result<(), ReplicationError> {
    exchange.validate(authority)?;
    let store = store.lock().map_err(|_| ReplicationError::LockPoisoned)?;
    for operation in exchange.operations {
        store.apply_membership(&operation, authority)?;
    }
    Ok(())
}

async fn write_exchange(
    stream: &mut TcpStream,
    exchange: &Exchange,
) -> Result<(), ReplicationError> {
    let payload = serde_json::to_vec(exchange)?;
    if payload.len() > MAX_REPLICATION_FRAME_BYTES as usize {
        return Err(ReplicationError::FrameTooLarge);
    }
    stream
        .write_all(&(payload.len() as u32).to_be_bytes())
        .await?;
    stream.write_all(&payload).await?;
    stream.flush().await?;
    Ok(())
}

async fn read_exchange(stream: &mut TcpStream) -> Result<Exchange, ReplicationError> {
    let mut length = [0; 4];
    stream.read_exact(&mut length).await?;
    let length = u32::from_be_bytes(length);
    if length > MAX_REPLICATION_FRAME_BYTES {
        return Err(ReplicationError::FrameTooLarge);
    }
    let mut payload = vec![0; length as usize];
    stream.read_exact(&mut payload).await?;
    Ok(serde_json::from_slice(&payload)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::membership::{MembershipRecord, MembershipState, SignedMembership};
    use ed25519_dalek::SigningKey;
    use std::net::Ipv4Addr;
    use tempfile::tempdir;

    fn fixture() -> (Arc<AuthorityKeyring>, SigningKey) {
        let key = SigningKey::from_bytes(&[4; 32]);
        let mut authority = AuthorityKeyring::new("project", 1);
        authority.add_authority("root", key.verifying_key());
        (Arc::new(authority), key)
    }

    fn operation(key: &SigningKey) -> SignedMembership {
        SignedMembership::sign(
            MembershipRecord {
                project_id: "project".into(),
                recovery_epoch: 1,
                protocol_version: MEMBERSHIP_PROTOCOL_VERSION,
                schema_version: MEMBERSHIP_SCHEMA_VERSION,
                node_id: "node-a".into(),
                server_name: "node-a".into(),
                node_signing_public_key: vec![5; 32],
                wireguard_public_key: "wg-a".into(),
                management_address: Ipv4Addr::new(100, 98, 64, 1),
                container_subnet: "198.18.1.0/24".into(),
                endpoints: vec!["192.0.2.1:51820".parse().unwrap()],
                owner_epoch: 1,
                revision: 1,
                state: MembershipState::Active,
            },
            "root",
            key,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn one_peer_write_reaches_another_without_cli_fanout() {
        let (authority, key) = fixture();
        let left_dir = tempdir().unwrap();
        let right_dir = tempdir().unwrap();
        let left = Arc::new(Mutex::new(
            AgentStore::open(&left_dir.path().join("agent.sqlite3")).unwrap(),
        ));
        let right = Arc::new(Mutex::new(
            AgentStore::open(&right_dir.path().join("agent.sqlite3")).unwrap(),
        ));
        left.lock()
            .unwrap()
            .apply_membership(&operation(&key), &authority)
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(serve(listener, Arc::clone(&right), Arc::clone(&authority)));
        sync_once(address, Arc::clone(&left), Arc::clone(&authority))
            .await
            .unwrap();
        assert_eq!(right.lock().unwrap().active_membership().unwrap().len(), 1);
        server.abort();
    }

    #[tokio::test]
    async fn peer_that_accepts_without_reply_is_bounded_by_exchange_timeout() {
        let (authority, _) = fixture();
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

        assert!(matches!(result, Err(ReplicationError::Timeout)));
        stalled_peer.abort();
    }

    #[tokio::test]
    async fn wrong_project_exchange_is_rejected_before_applying_operations() {
        let (authority, _) = fixture();
        let dir = tempdir().unwrap();
        let store = Arc::new(Mutex::new(
            AgentStore::open(&dir.path().join("agent.sqlite3")).unwrap(),
        ));
        let exchange = Exchange {
            project_id: "other".into(),
            recovery_epoch: 1,
            protocol_version: MEMBERSHIP_PROTOCOL_VERSION,
            schema_version: MEMBERSHIP_SCHEMA_VERSION,
            operations: vec![],
        };
        assert!(matches!(
            apply_exchange(exchange, &store, &authority),
            Err(ReplicationError::WrongProject)
        ));
    }

    /// Phase 8 exit criterion: "mixed-version nodes are rejected rather than partially joined."
    /// ADR 0002 requires the first release to support exactly protocol 1/schema 1 and reject
    /// every other version before exchanging state -- there is no mixed-version cluster.
    #[tokio::test]
    async fn mismatched_protocol_version_is_rejected_before_applying_operations() {
        let (authority, _) = fixture();
        let dir = tempdir().unwrap();
        let store = Arc::new(Mutex::new(
            AgentStore::open(&dir.path().join("agent.sqlite3")).unwrap(),
        ));
        let exchange = Exchange {
            project_id: authority.project_id().to_string(),
            recovery_epoch: authority.recovery_epoch(),
            protocol_version: MEMBERSHIP_PROTOCOL_VERSION + 1,
            schema_version: MEMBERSHIP_SCHEMA_VERSION,
            operations: vec![],
        };
        assert!(matches!(
            apply_exchange(exchange, &store, &authority),
            Err(ReplicationError::IncompatibleVersion)
        ));
        assert!(store
            .lock()
            .unwrap()
            .active_membership()
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn mismatched_schema_version_is_rejected_before_applying_operations() {
        let (authority, _) = fixture();
        let dir = tempdir().unwrap();
        let store = Arc::new(Mutex::new(
            AgentStore::open(&dir.path().join("agent.sqlite3")).unwrap(),
        ));
        let exchange = Exchange {
            project_id: authority.project_id().to_string(),
            recovery_epoch: authority.recovery_epoch(),
            protocol_version: MEMBERSHIP_PROTOCOL_VERSION,
            schema_version: MEMBERSHIP_SCHEMA_VERSION + 1,
            operations: vec![],
        };
        assert!(matches!(
            apply_exchange(exchange, &store, &authority),
            Err(ReplicationError::IncompatibleVersion)
        ));
        assert!(store
            .lock()
            .unwrap()
            .active_membership()
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn peer_that_was_offline_converges_after_it_returns() {
        let (authority, key) = fixture();
        let source_dir = tempdir().unwrap();
        let returning_dir = tempdir().unwrap();
        let source = Arc::new(Mutex::new(
            AgentStore::open(&source_dir.path().join("agent.sqlite3")).unwrap(),
        ));
        let returning = Arc::new(Mutex::new(
            AgentStore::open(&returning_dir.path().join("agent.sqlite3")).unwrap(),
        ));
        source
            .lock()
            .unwrap()
            .apply_membership(&operation(&key), &authority)
            .unwrap();

        let unavailable: std::net::SocketAddr = "127.0.0.1:9".parse().unwrap();
        assert!(
            sync_once(unavailable, Arc::clone(&source), Arc::clone(&authority))
                .await
                .is_err()
        );
        assert!(returning
            .lock()
            .unwrap()
            .active_membership()
            .unwrap()
            .is_empty());

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(serve(
            listener,
            Arc::clone(&returning),
            Arc::clone(&authority),
        ));
        sync_once(address, Arc::clone(&source), Arc::clone(&authority))
            .await
            .unwrap();
        assert_eq!(
            returning.lock().unwrap().active_membership().unwrap().len(),
            1
        );
        server.abort();
    }

    #[tokio::test]
    async fn long_offline_peer_converges_from_compacted_winning_snapshot() {
        let (authority, key) = fixture();
        let source_dir = tempdir().unwrap();
        let returning_dir = tempdir().unwrap();
        let source = Arc::new(Mutex::new(
            AgentStore::open(&source_dir.path().join("agent.sqlite3")).unwrap(),
        ));
        let returning = Arc::new(Mutex::new(
            AgentStore::open(&returning_dir.path().join("agent.sqlite3")).unwrap(),
        ));
        let first = operation(&key);
        source
            .lock()
            .unwrap()
            .apply_membership(&first, &authority)
            .unwrap();
        let mut newer_record = first.record.clone();
        newer_record.revision = 2;
        newer_record.endpoints = vec!["192.0.2.99:51820".parse().unwrap()];
        let newer = SignedMembership::sign(newer_record, "root", &key).unwrap();
        source
            .lock()
            .unwrap()
            .apply_membership(&newer, &authority)
            .unwrap();
        assert_eq!(
            source
                .lock()
                .unwrap()
                .compact_operations()
                .unwrap()
                .membership_removed,
            1
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(serve(
            listener,
            Arc::clone(&returning),
            Arc::clone(&authority),
        ));
        sync_once(address, Arc::clone(&source), Arc::clone(&authority))
            .await
            .unwrap();
        let records = returning.lock().unwrap().latest_membership().unwrap();
        assert_eq!(records[0].revision, 2);
        assert_eq!(
            records[0].endpoints,
            vec!["192.0.2.99:51820".parse().unwrap()]
        );
        server.abort();
    }
}
