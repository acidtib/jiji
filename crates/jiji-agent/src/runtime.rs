//! Authoritative membership replication, catalog replication, DNS, and incremental WireGuard
//! repair -- everything that depends on the mesh's replicated peer set.

use std::collections::BTreeMap;
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::process::Command;

use crate::catalog_replication;
use crate::dns::{self, DnsConfig};
use crate::membership::AuthorityKeyring;
use crate::replication;
use crate::store::{AgentStore, StoreError};
use crate::wireguard::{plan_reconciliation, PeerAction};

/// A peer's replicas are suppressed from DNS once its last successful anti-entropy exchange is
/// older than this many multiples of `reconcile_interval_secs` -- generous enough that ordinary
/// tick jitter never flaps a healthy peer's eligibility, see `dns.rs`'s reachability filter.
const REACHABILITY_TIMEOUT_INTERVALS: u64 = 3;
pub const DEFAULT_STORE_SOFT_QUOTA_BYTES: u64 = 64 * 1024 * 1024;
pub const DEFAULT_COMPACTION_INTERVAL_SECS: u64 = 300;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorityConfig {
    pub id: String,
    pub public_key: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalRuntimeConfig {
    pub bridge_network: String,
    pub bridge_interface: String,
    pub proxy_address: std::net::Ipv4Addr,
    #[serde(default)]
    pub proxy_routes: Vec<ProxyRouteSpec>,
    /// Everything below is what Phase 9's native bridge/DNS bring-up (`bridge_bringup.rs`) needs
    /// to render the same script `jiji-network-restore-{slug}.service` used to run, without the
    /// agent depending on that unit -- see `jiji_network::render_restore_script`.
    pub container_subnet: jiji_network::Ipv4Cidr,
    pub bridge_gateway: std::net::Ipv4Addr,
    pub container_cidr: jiji_network::Ipv4Cidr,
    pub wireguard_port: u16,
    /// Each configured peer's WireGuard endpoint host, pre-parsed and validated as a public IPv4
    /// address by the CLI (`BridgeProvisioner::peer_public_ips`) so this module never has to.
    #[serde(default)]
    pub peer_public_ips: Vec<std::net::Ipv4Addr>,
    /// Used only inside actionable error messages the rendered script prints on drift.
    pub public_host: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyRouteSpec {
    pub service: String,
    pub route_name: String,
    pub port: u32,
    /// Static kamal-proxy deploy arguments (hosts, TLS, path and health policy). Dynamic
    /// `--target` arguments are rebuilt from the healthy catalog on every repair.
    pub deploy_args: Vec<String>,
    pub deploy_timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshConfig {
    pub project_id: String,
    pub recovery_epoch: u64,
    pub node_id: String,
    /// Node-local signing seed, installed mode 0600 and never replicated.
    /// Phase 4 uses it to sign service/catalog ownership records.
    pub node_signing_key: Vec<u8>,
    pub wireguard_interface: String,
    /// Path to the WireGuard private key file `jiji network setup` already generates and installs
    /// mode 0600, root-owned (`{network_dir}/private.key`). The agent passes this path straight to
    /// `wg set ... private-key <path>` for native interface bring-up (Phase 9) -- the key value
    /// itself is never round-tripped through this config file or the CLI's own process; only the
    /// path is.
    pub wireguard_private_key_path: std::path::PathBuf,
    pub replication_bind: SocketAddr,
    /// Project bridge address used by service containers as their resolver.
    pub dns_bind_address: std::net::Ipv4Addr,
    pub local_runtime: LocalRuntimeConfig,
    /// Public/bootstrap seeds used before authenticated membership is available.
    /// They are hints, never membership authority.
    #[serde(default)]
    pub replication_peers: Vec<SocketAddr>,
    pub authorities: Vec<AuthorityConfig>,
    #[serde(default = "default_interval")]
    pub reconcile_interval_secs: u64,
    #[serde(default = "default_store_soft_quota_bytes")]
    pub store_soft_quota_bytes: u64,
    #[serde(default = "default_compaction_interval")]
    pub compaction_interval_secs: u64,
    /// Where the DNS resolver forwards any query outside this project's own `.jiji` zone (see
    /// `dns::DnsConfig::forwarders`). Defaulted so an existing installation's already-written
    /// `mesh_config.json` (from before this field existed) still deserializes cleanly on upgrade.
    #[serde(default = "default_dns_forwarders")]
    pub dns_forwarders: Vec<std::net::Ipv4Addr>,
}

fn default_interval() -> u64 {
    10
}

fn default_dns_forwarders() -> Vec<std::net::Ipv4Addr> {
    vec![
        std::net::Ipv4Addr::new(1, 1, 1, 1),
        std::net::Ipv4Addr::new(8, 8, 8, 8),
    ]
}

fn default_store_soft_quota_bytes() -> u64 {
    DEFAULT_STORE_SOFT_QUOTA_BYTES
}

fn default_compaction_interval() -> u64 {
    DEFAULT_COMPACTION_INTERVAL_SECS
}

impl MeshConfig {
    pub fn peer_reachability_timeout_secs(&self) -> u64 {
        self.reconcile_interval_secs
            .saturating_mul(REACHABILITY_TIMEOUT_INTERVALS)
    }

    pub fn load(path: &Path, expected_project: &str) -> Result<Self, RuntimeError> {
        let config = serde_json::from_slice::<Self>(&std::fs::read(path)?)?;
        config.validate(expected_project)?;
        Ok(config)
    }

    pub fn validate(&self, expected_project: &str) -> Result<(), RuntimeError> {
        if self.project_id != expected_project {
            return Err(RuntimeError::WrongProject);
        }
        if self.project_id.is_empty()
            || self.node_id.is_empty()
            || self.wireguard_interface.is_empty()
            || self.wireguard_private_key_path.as_os_str().is_empty()
            || self.local_runtime.bridge_network.is_empty()
            || self.local_runtime.bridge_interface.is_empty()
            || self.local_runtime.wireguard_port == 0
            || self.local_runtime.public_host.is_empty()
            || self.node_signing_key.len() != 32
            || self.recovery_epoch == 0
            || self.reconcile_interval_secs == 0
            || self.store_soft_quota_bytes == 0
            || self.compaction_interval_secs == 0
            || self.authorities.is_empty()
        {
            return Err(RuntimeError::InvalidConfig);
        }
        self.keyring()?;
        Ok(())
    }

    pub fn keyring(&self) -> Result<AuthorityKeyring, RuntimeError> {
        let mut keyring = AuthorityKeyring::new(&self.project_id, self.recovery_epoch);
        for authority in &self.authorities {
            let bytes: [u8; 32] = authority
                .public_key
                .as_slice()
                .try_into()
                .map_err(|_| RuntimeError::InvalidAuthorityKey)?;
            let key =
                VerifyingKey::from_bytes(&bytes).map_err(|_| RuntimeError::InvalidAuthorityKey)?;
            keyring.add_authority(&authority.id, key);
        }
        Ok(keyring)
    }
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("mesh runtime i/o failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("mesh runtime configuration is invalid")]
    InvalidConfig,
    #[error("mesh runtime configuration belongs to another project")]
    WrongProject,
    #[error("membership authority public key is invalid")]
    InvalidAuthorityKey,
    #[error("mesh runtime configuration could not be decoded: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("agent store failed: {0}")]
    Store(#[from] StoreError),
    #[error("agent store lock is poisoned")]
    LockPoisoned,
    #[error("WireGuard inspection failed: {0}")]
    WireGuardInspect(String),
    #[error("WireGuard reconciliation failed: {0}")]
    WireGuardApply(String),
    #[error("catalog replication failed: {0}")]
    CatalogReplication(#[from] catalog_replication::CatalogReplicationError),
    #[error("dns server failed: {0}")]
    Dns(#[from] dns::DnsError),
    #[error("peer synchronization task failed: {0}")]
    PeerSyncTask(String),
}

pub async fn run(config: MeshConfig, store: Arc<Mutex<AgentStore>>) -> Result<(), RuntimeError> {
    let authority = Arc::new(config.keyring()?);
    let catalog_port = jiji_network::catalog_replication_port(&config.project_id);
    let catalog_bind = SocketAddr::new(config.replication_bind.ip(), catalog_port);
    let dns_bind = SocketAddr::new(config.dns_bind_address.into(), 53);

    // Hydrate and repair last-known-good peers before network-dependent
    // replication. Cold boot therefore does not depend on peer availability.
    if let Err(error) = reconcile_once(&config, &store).await {
        tracing::warn!(%error, "cold-start WireGuard repair deferred");
    }
    // This node is always reachable to itself, independent of any peer replication succeeding.
    if let Err(error) = mark_seen(&store, &config.node_id) {
        tracing::warn!(%error, "could not record local node liveness");
    }

    let listener = TcpListener::bind(config.replication_bind).await?;
    let server = tokio::spawn(replication::serve(
        listener,
        Arc::clone(&store),
        Arc::clone(&authority),
    ));
    let catalog_listener = TcpListener::bind(catalog_bind).await?;
    let catalog_server = tokio::spawn(catalog_replication::serve(
        catalog_listener,
        Arc::clone(&store),
        Arc::clone(&authority),
    ));
    let dns_config = DnsConfig {
        project_id: config.project_id.clone(),
        recovery_epoch: config.recovery_epoch,
        local_node_id: config.node_id.clone(),
        reachability_timeout: Duration::from_secs(config.peer_reachability_timeout_secs()),
        forwarders: config
            .dns_forwarders
            .iter()
            .map(|address| SocketAddr::new((*address).into(), 53))
            .collect(),
    };
    let dns_server = tokio::spawn(dns::serve(
        dns_bind,
        Arc::clone(&store),
        Arc::clone(&authority),
        dns_config,
    ));
    let mut interval = tokio::time::interval(Duration::from_secs(config.reconcile_interval_secs));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_compaction = tokio::time::Instant::now();

    loop {
        interval.tick().await;
        let peers = replication_targets(&config, &store)?;
        let current_peer_ids = peers
            .iter()
            .map(|(node_id, _)| node_id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        {
            let store = store.lock().map_err(|_| RuntimeError::LockPoisoned)?;
            for status in store.peer_sync_statuses()? {
                if !current_peer_ids.contains(status.node_id.as_str()) {
                    store.delete_peer_sync_status(&status.node_id)?;
                }
            }
        }
        let mut syncs = tokio::task::JoinSet::new();
        for (node_id, membership_addr) in peers {
            let store = Arc::clone(&store);
            let authority = Arc::clone(&authority);
            syncs.spawn(async move {
                let catalog_addr = SocketAddr::new(membership_addr.ip(), catalog_port);
                let (membership, catalog) = tokio::join!(
                    replication::sync_once(
                        membership_addr,
                        Arc::clone(&store),
                        Arc::clone(&authority),
                    ),
                    catalog_replication::sync_once(catalog_addr, store, authority),
                );
                (node_id, membership_addr, catalog_addr, membership, catalog)
            });
        }
        while let Some(result) = syncs.join_next().await {
            let (node_id, membership_addr, catalog_addr, membership, catalog) =
                result.map_err(|error| RuntimeError::PeerSyncTask(error.to_string()))?;
            let mut reached = false;
            let mut failures = Vec::new();
            if let Err(error) = membership {
                tracing::debug!(%membership_addr, %error, "membership peer temporarily unavailable");
                failures.push(format!("membership: {error}"));
            } else {
                reached = true;
            }
            if let Err(error) = catalog {
                tracing::debug!(%catalog_addr, %error, "catalog peer temporarily unavailable");
                failures.push(format!("catalog: {error}"));
            } else {
                reached = true;
            }
            if reached {
                if let Err(error) = mark_seen(&store, &node_id) {
                    tracing::warn!(%error, %node_id, "could not record peer liveness");
                }
            }
            let status_result = if failures.is_empty() {
                store
                    .lock()
                    .map_err(|_| RuntimeError::LockPoisoned)?
                    .record_peer_sync_success(&node_id)
            } else {
                store
                    .lock()
                    .map_err(|_| RuntimeError::LockPoisoned)?
                    .record_peer_sync_failure(&node_id, &failures.join("; "))
            };
            if let Err(error) = status_result {
                tracing::warn!(%error, %node_id, "could not record peer sync status");
            }
        }
        if last_compaction.elapsed() >= Duration::from_secs(config.compaction_interval_secs) {
            match store
                .lock()
                .map_err(|_| RuntimeError::LockPoisoned)?
                .compact_operations()
            {
                Ok(result) => {
                    tracing::info!(
                        membership_removed = result.membership_removed,
                        catalog_removed = result.catalog_removed,
                        desired_removed = result.desired_removed,
                        "replicated operation history compacted"
                    );
                    last_compaction = tokio::time::Instant::now();
                }
                Err(error) => tracing::warn!(%error, "operation compaction deferred"),
            }
        }
        if let Err(error) = reconcile_once(&config, &store).await {
            tracing::warn!(%error, "incremental WireGuard reconciliation failed");
        }
        if server.is_finished() {
            return Err(RuntimeError::WireGuardApply(
                "membership replication listener stopped unexpectedly".into(),
            ));
        }
        if catalog_server.is_finished() {
            return Err(RuntimeError::WireGuardApply(
                "catalog replication listener stopped unexpectedly".into(),
            ));
        }
        if dns_server.is_finished() {
            return Err(RuntimeError::WireGuardApply(
                "dns server stopped unexpectedly".into(),
            ));
        }
    }
}

fn mark_seen(store: &Arc<Mutex<AgentStore>>, node_id: &str) -> Result<(), RuntimeError> {
    store
        .lock()
        .map_err(|_| RuntimeError::LockPoisoned)?
        .mark_node_seen(node_id)?;
    Ok(())
}

fn replication_targets(
    config: &MeshConfig,
    store: &Arc<Mutex<AgentStore>>,
) -> Result<Vec<(String, SocketAddr)>, RuntimeError> {
    let mut peers = std::collections::BTreeMap::new();
    let port = config.replication_bind.port();
    for peer in &config.replication_peers {
        // Bootstrap seeds predate authenticated membership, so their node ID isn't known yet;
        // keyed by address alone until membership resolves a real ID for them.
        peers.insert(*peer, peer.to_string());
    }
    for record in store
        .lock()
        .map_err(|_| RuntimeError::LockPoisoned)?
        .active_membership()?
    {
        if record.node_id != config.node_id {
            let address = SocketAddr::new(record.management_address.into(), port);
            peers.insert(address, record.node_id.clone());
        }
    }
    peers.remove(&config.replication_bind);
    Ok(peers
        .into_iter()
        .map(|(address, node_id)| (node_id, address))
        .collect())
}

pub async fn reconcile_once(
    config: &MeshConfig,
    store: &Arc<Mutex<AgentStore>>,
) -> Result<(), RuntimeError> {
    let (records, cache) = {
        let store = store.lock().map_err(|_| RuntimeError::LockPoisoned)?;
        (store.latest_membership()?, store.peer_cache()?)
    };
    let observed = observed_endpoints(&config.wireguard_interface).await?;
    let plan = plan_reconciliation(&config.node_id, &records, &cache, &observed);
    for action in &plan.actions {
        apply_action(&config.wireguard_interface, action).await?;
    }
    // Never advance last-known-good state if a command failed.
    store
        .lock()
        .map_err(|_| RuntimeError::LockPoisoned)?
        .replace_peer_cache(&plan.next_cache)?;
    Ok(())
}

async fn observed_endpoints(interface: &str) -> Result<BTreeMap<String, SocketAddr>, RuntimeError> {
    let output = Command::new("wg")
        .args(["show", interface, "endpoints"])
        .output()
        .await?;
    if !output.status.success() {
        return Err(RuntimeError::WireGuardInspect(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    let mut endpoints = BTreeMap::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut fields = line.split_whitespace();
        let (Some(public_key), Some(endpoint)) = (fields.next(), fields.next()) else {
            continue;
        };
        if endpoint == "(none)" {
            continue;
        }
        if let Ok(mut resolved) = endpoint.to_socket_addrs() {
            if let Some(address) = resolved.next() {
                endpoints.insert(public_key.to_string(), address);
            }
        }
    }
    Ok(endpoints)
}

async fn apply_action(interface: &str, action: &PeerAction) -> Result<(), RuntimeError> {
    let mut command = Command::new("wg");
    command.args(["set", interface, "peer"]);
    match action {
        PeerAction::Set {
            public_key,
            endpoint,
            allowed_ips,
            ..
        } => {
            command
                .arg(public_key)
                .arg("endpoint")
                .arg(endpoint.to_string())
                .arg("allowed-ips")
                .arg(allowed_ips.join(","))
                .args(["persistent-keepalive", "25"]);
        }
        PeerAction::UpdateEndpoint {
            public_key,
            endpoint,
            ..
        } => {
            command
                .arg(public_key)
                .arg("endpoint")
                .arg(endpoint.to_string());
        }
        PeerAction::Remove { public_key, .. } => {
            command.arg(public_key).arg("remove");
        }
    }
    let output = command.output().await?;
    if output.status.success() {
        Ok(())
    } else {
        Err(RuntimeError::WireGuardApply(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    fn config() -> MeshConfig {
        let key = SigningKey::from_bytes(&[8; 32]);
        MeshConfig {
            project_id: "demo".into(),
            recovery_epoch: 1,
            node_id: "node-a".into(),
            node_signing_key: vec![9; 32],
            wireguard_interface: "jijitest".into(),
            wireguard_private_key_path: "/etc/jiji/network/demo/private.key".into(),
            replication_bind: "127.0.0.1:17444".parse().unwrap(),
            dns_bind_address: "127.0.0.2".parse().unwrap(),
            local_runtime: LocalRuntimeConfig {
                bridge_network: "jiji-demo".into(),
                bridge_interface: "jijibdemo".into(),
                proxy_address: "127.0.0.3".parse().unwrap(),
                proxy_routes: vec![],
                container_subnet: "198.18.2.0/24".parse().unwrap(),
                bridge_gateway: "198.18.2.1".parse().unwrap(),
                container_cidr: "198.18.0.0/16".parse().unwrap(),
                wireguard_port: 51820,
                peer_public_ips: vec![],
                public_host: "203.0.113.10".into(),
            },
            replication_peers: vec![],
            authorities: vec![AuthorityConfig {
                id: "root".into(),
                public_key: key.verifying_key().to_bytes().to_vec(),
            }],
            reconcile_interval_secs: 10,
            store_soft_quota_bytes: DEFAULT_STORE_SOFT_QUOTA_BYTES,
            compaction_interval_secs: DEFAULT_COMPACTION_INTERVAL_SECS,
            dns_forwarders: default_dns_forwarders(),
        }
    }

    #[test]
    fn config_rejects_cross_project_and_invalid_authority() {
        assert!(matches!(
            config().validate("other"),
            Err(RuntimeError::WrongProject)
        ));
        let mut invalid = config();
        invalid.authorities[0].public_key = vec![1; 3];
        assert!(matches!(
            invalid.validate("demo"),
            Err(RuntimeError::InvalidAuthorityKey)
        ));
    }

    #[test]
    fn valid_config_accepts_overlapping_authority_keys() {
        let mut valid = config();
        valid.authorities.push(valid.authorities[0].clone());
        assert!(valid.validate("demo").is_ok());
    }

    #[test]
    fn authenticated_membership_becomes_replication_targets() {
        use crate::membership::{
            MembershipRecord, MembershipState, SignedMembership, MEMBERSHIP_PROTOCOL_VERSION,
            MEMBERSHIP_SCHEMA_VERSION,
        };
        use std::net::Ipv4Addr;
        use tempfile::tempdir;

        let config = config();
        let key = SigningKey::from_bytes(&[8; 32]);
        let authority = config.keyring().unwrap();
        let operation = SignedMembership::sign(
            MembershipRecord {
                project_id: "demo".into(),
                recovery_epoch: 1,
                protocol_version: MEMBERSHIP_PROTOCOL_VERSION,
                schema_version: MEMBERSHIP_SCHEMA_VERSION,
                node_id: "node-b".into(),
                server_name: "node-b".into(),
                node_signing_public_key: vec![3; 32],
                wireguard_public_key: "wg-b".into(),
                management_address: Ipv4Addr::new(100, 98, 64, 2),
                container_subnet: "198.18.2.0/24".into(),
                endpoints: vec!["192.0.2.2:51820".parse().unwrap()],
                owner_epoch: 1,
                revision: 1,
                state: MembershipState::Active,
            },
            "root",
            &key,
        )
        .unwrap();
        let dir = tempdir().unwrap();
        let store = Arc::new(Mutex::new(
            AgentStore::open(&dir.path().join("agent.sqlite3")).unwrap(),
        ));
        store
            .lock()
            .unwrap()
            .apply_membership(&operation, &authority)
            .unwrap();

        assert_eq!(
            replication_targets(&config, &store).unwrap(),
            vec![("node-b".to_string(), "100.98.64.2:17444".parse().unwrap())]
        );
    }
}
