//! Authoritative membership replication, catalog replication, DNS, and incremental WireGuard
//! repair -- everything that depends on the mesh's replicated peer set.

use std::collections::BTreeMap;
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::process::Command;

use crate::catalog_replication;
use crate::dns::{self, DnsConfig};
use crate::membership::{MembershipScope, NodeIdentity};
use crate::store::{AgentStore, StoreError};
use crate::wireguard::{plan_reconciliation, PeerAction};

/// A peer's replicas are suppressed from DNS once its last successful anti-entropy exchange is
/// older than this many multiples of `reconcile_interval_secs` -- generous enough that ordinary
/// tick jitter never flaps a healthy peer's eligibility, see `dns.rs`'s reachability filter.
const REACHABILITY_TIMEOUT_INTERVALS: u64 = 3;
pub const DEFAULT_STORE_SOFT_QUOTA_BYTES: u64 = 64 * 1024 * 1024;
pub const DEFAULT_COMPACTION_INTERVAL_SECS: u64 = 300;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalRuntimeConfig {
    pub bridge_network: String,
    pub bridge_interface: String,
    pub proxy_address: std::net::Ipv4Addr,
    #[serde(default)]
    pub proxy_routes: Vec<ProxyRouteSpec>,
    #[serde(default)]
    pub tcp_routes: Vec<TcpRouteSpec>,
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

/// One route jiji-proxy should serve for a service on this host, computed
/// once by jiji-cli (`proxy_routes::runtime_specs_for_service`) and shipped
/// as part of the mesh config -- unlike kamal-proxy's route model, this
/// carries no explicit target address at all: jiji-proxy resolves and
/// load-balances `name`'s backends itself, continuously, so applying this
/// spec doesn't need to change per deployment (see "Core design decision"
/// in plans/jiji-proxy-design.md). `local_reconcile.rs` fills in
/// `--dns-server` itself (always this agent's own local `.jiji` resolver
/// address) when rendering the actual `jiji-proxy route apply` command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyRouteSpec {
    pub host: String,
    #[serde(default)]
    pub path_prefix: Option<String>,
    /// The aggregate DNS name jiji-proxy re-resolves on its own schedule:
    /// `{project}-{service}.jiji`.
    pub name: String,
    pub port: u16,
    /// Static `jiji-proxy route apply` policy flags (`--tls`,
    /// `--health-check`...) -- everything except host/dns-server/name/
    /// port/path-prefix, which are already explicit fields above.
    #[serde(default)]
    pub apply_args: Vec<String>,
}

/// A raw TCP route jiji-proxy should relay for a service on this host,
/// computed once by jiji-cli (`proxy_routes::runtime_specs_for_service`) and
/// shipped as part of the mesh config -- mirrors `ProxyRouteSpec` for TCP
/// mode (see `crate::tcp_relay` in the jiji-proxy crate): keyed by
/// `listen_port` instead of `host`/`path_prefix`, since raw TCP has no Host
/// header to route by. Also feeds `proxy_bringup::reconcile`'s ingress
/// nftables rendering directly: the desired `listen_port` set here is what
/// gets DNAT'd, kept correct by the same idempotent-reapply-every-tick
/// pattern `reconcile_proxy_routes` already uses for HTTP, so there is no
/// need to separately query jiji-proxy's own live route table just to learn
/// the ingress port list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TcpRouteSpec {
    pub listen_port: u16,
    /// The aggregate DNS name jiji-proxy re-resolves on its own schedule:
    /// `{project}-{service}.jiji`.
    pub name: String,
    pub port: u16,
    /// Static `jiji-proxy tcp-route apply` policy flags (`--health-check`...)
    /// -- everything except listen-port/dns-server/name/port, which are
    /// already explicit fields above.
    #[serde(default)]
    pub apply_args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshConfig {
    pub project_id: String,
    pub recovery_epoch: u64,
    pub node_id: String,
    pub wireguard_interface: String,
    /// Path to the WireGuard private key file `jiji network setup` already generates and installs
    /// mode 0600, root-owned (`{network_dir}/private.key`). The agent passes this path straight to
    /// `wg set ... private-key <path>` for native interface bring-up (Phase 9) -- the key value
    /// itself is never round-tripped through this config file or the CLI's own process; only the
    /// path is.
    pub wireguard_private_key_path: std::path::PathBuf,
    /// This node's own catalog/desired-state anti-entropy bind address: management address plus
    /// `jiji_network::catalog_replication_port`. Membership has no listener of its own -- it's
    /// pushed directly by `jiji-cli` over SSH (`jiji-agent membership-import`), never gossiped, so
    /// there's nothing to bind for it (see `membership.rs`'s module doc comment).
    pub replication_bind: SocketAddr,
    /// Project bridge address used by service containers as their resolver.
    pub dns_bind_address: std::net::Ipv4Addr,
    pub local_runtime: LocalRuntimeConfig,
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
            || self.recovery_epoch == 0
            || self.reconcile_interval_secs == 0
            || self.store_soft_quota_bytes == 0
            || self.compaction_interval_secs == 0
        {
            return Err(RuntimeError::InvalidConfig);
        }
        Ok(())
    }

    /// This node's own management address, used both to bind the catalog/desired-state listener
    /// and (explicitly, rather than trusting default route selection) to source outbound
    /// replication connections -- see `catalog_replication.rs`'s module doc comment for why the
    /// source address matters.
    pub fn management_address(&self) -> std::net::Ipv4Addr {
        match self.replication_bind.ip() {
            std::net::IpAddr::V4(address) => address,
            std::net::IpAddr::V6(_) => {
                unreachable!(
                    "jiji's mesh is IPv4-only; replication_bind is always Ipv4Addr-derived"
                )
            }
        }
    }

    pub fn identity(&self) -> NodeIdentity {
        NodeIdentity {
            project_id: self.project_id.clone(),
            recovery_epoch: self.recovery_epoch,
            node_id: self.node_id.clone(),
        }
    }

    pub fn scope(&self) -> MembershipScope {
        MembershipScope::new(self.project_id.clone(), self.recovery_epoch)
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
    let identity = Arc::new(config.identity());
    let local_address = config.management_address();
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

    let catalog_listener = TcpListener::bind(config.replication_bind).await?;
    let catalog_server = tokio::spawn(catalog_replication::serve(
        catalog_listener,
        Arc::clone(&store),
        Arc::clone(&identity),
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
    let dns_server = tokio::spawn(dns::serve(dns_bind, Arc::clone(&store), dns_config));
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
        for (node_id, catalog_addr) in peers {
            let store = Arc::clone(&store);
            let identity = Arc::clone(&identity);
            syncs.spawn(async move {
                let catalog =
                    catalog_replication::sync_once(catalog_addr, local_address, store, identity)
                        .await;
                (node_id, catalog_addr, catalog)
            });
        }
        while let Some(result) = syncs.join_next().await {
            let (node_id, catalog_addr, catalog) =
                result.map_err(|error| RuntimeError::PeerSyncTask(error.to_string()))?;
            let mut reached = false;
            let mut failures = Vec::new();
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

/// Every other active member's catalog/desired-state replication address. Unlike the
/// pre-Phase-N design there is no bootstrap-seed chicken-and-egg problem to solve here: `jiji-cli`
/// installs the complete membership set directly (`jiji-agent membership-import`, see
/// `membership.rs`) before this agent ever needs to talk to a peer, so `active_membership()`
/// already has everything on the very first tick.
fn replication_targets(
    config: &MeshConfig,
    store: &Arc<Mutex<AgentStore>>,
) -> Result<Vec<(String, SocketAddr)>, RuntimeError> {
    let port = config.replication_bind.port();
    let mut peers = std::collections::BTreeMap::new();
    for record in store
        .lock()
        .map_err(|_| RuntimeError::LockPoisoned)?
        .active_membership()?
    {
        if record.node_id != config.node_id {
            let address = SocketAddr::new(record.management_address.into(), port);
            peers.insert(record.node_id.clone(), address);
        }
    }
    Ok(peers.into_iter().collect())
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
    if !output.status.success() {
        return Err(RuntimeError::WireGuardApply(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    match action {
        PeerAction::Set { allowed_ips, .. } => replace_routes(interface, allowed_ips).await,
        PeerAction::Remove { allowed_ips, .. } => {
            delete_routes(interface, allowed_ips).await;
            Ok(())
        }
        PeerAction::UpdateEndpoint { .. } => Ok(()),
    }
}

/// `wg set ... allowed-ips` never touches the OS routing table on its own (see this module's own
/// doc comment) -- every peer's routes are resynced here, on the exact same reconcile pass that
/// already applies its `wg set`, so a peer's routes self-heal on cold boot (a fresh `Set` action,
/// forced by `local_reconcile.rs::ensure_link` clearing the peer cache whenever it reconfigures the
/// link) exactly the same way an ordinary membership change does. `ip route replace` is idempotent,
/// so a route that's already correct is a harmless no-op; failure here is treated as seriously as a
/// failed `wg set`, since a peer with no route is a peer this host cannot actually reach.
async fn replace_routes(interface: &str, allowed_ips: &[String]) -> Result<(), RuntimeError> {
    for ip in allowed_ips {
        let output = Command::new("ip")
            .args(["route", "replace", ip, "dev", interface])
            .output()
            .await?;
        if !output.status.success() {
            return Err(RuntimeError::WireGuardApply(format!(
                "could not install route for {ip} via {interface}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
    }
    Ok(())
}

/// Best-effort cleanup, deliberately non-fatal: the route may already be gone (a concurrent
/// reconcile, or it was never installed in the first place), and a peer whose `wg set ... remove`
/// already succeeded is removed regardless of whether its now-unreachable routes are tidied up.
async fn delete_routes(interface: &str, allowed_ips: &[String]) {
    for ip in allowed_ips {
        let result = Command::new("ip")
            .args(["route", "del", ip, "dev", interface])
            .output()
            .await;
        if !matches!(&result, Ok(output) if output.status.success()) {
            tracing::debug!(ip, interface, "could not remove route (already absent?)");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> MeshConfig {
        MeshConfig {
            project_id: "demo".into(),
            recovery_epoch: 1,
            node_id: "node-a".into(),
            wireguard_interface: "jijitest".into(),
            wireguard_private_key_path: "/etc/jiji/network/demo/private.key".into(),
            replication_bind: "127.0.0.1:17444".parse().unwrap(),
            dns_bind_address: "127.0.0.2".parse().unwrap(),
            local_runtime: LocalRuntimeConfig {
                bridge_network: "jiji-demo".into(),
                bridge_interface: "jijibdemo".into(),
                proxy_address: "127.0.0.3".parse().unwrap(),
                proxy_routes: vec![],
                tcp_routes: vec![],
                container_subnet: "198.18.2.0/24".parse().unwrap(),
                bridge_gateway: "198.18.2.1".parse().unwrap(),
                container_cidr: "198.18.0.0/16".parse().unwrap(),
                wireguard_port: 51820,
                peer_public_ips: vec![],
                public_host: "203.0.113.10".into(),
            },
            reconcile_interval_secs: 10,
            store_soft_quota_bytes: DEFAULT_STORE_SOFT_QUOTA_BYTES,
            compaction_interval_secs: DEFAULT_COMPACTION_INTERVAL_SECS,
            dns_forwarders: default_dns_forwarders(),
        }
    }

    #[test]
    fn config_rejects_cross_project() {
        assert!(matches!(
            config().validate("other"),
            Err(RuntimeError::WrongProject)
        ));
        assert!(config().validate("demo").is_ok());
    }

    #[test]
    fn membership_becomes_replication_targets() {
        use crate::membership::{
            MembershipRecord, MembershipScope, MembershipState, MEMBERSHIP_PROTOCOL_VERSION,
            MEMBERSHIP_SCHEMA_VERSION,
        };
        use std::net::Ipv4Addr;
        use tempfile::tempdir;

        let config = config();
        let scope = MembershipScope::new("demo", 1);
        let record = MembershipRecord {
            project_id: "demo".into(),
            recovery_epoch: 1,
            protocol_version: MEMBERSHIP_PROTOCOL_VERSION,
            schema_version: MEMBERSHIP_SCHEMA_VERSION,
            node_id: "node-b".into(),
            server_name: "node-b".into(),
            wireguard_public_key: "wg-b".into(),
            management_address: Ipv4Addr::new(100, 98, 64, 2),
            container_subnet: "198.18.2.0/24".into(),
            endpoints: vec!["192.0.2.2:51820".parse().unwrap()],
            owner_epoch: 1,
            revision: 1,
            state: MembershipState::Active,
        };
        let dir = tempdir().unwrap();
        let store = Arc::new(Mutex::new(
            AgentStore::open(&dir.path().join("agent.sqlite3")).unwrap(),
        ));
        store
            .lock()
            .unwrap()
            .apply_membership(record, &scope)
            .unwrap();

        assert_eq!(
            replication_targets(&config, &store).unwrap(),
            vec![("node-b".to_string(), "100.98.64.2:17444".parse().unwrap())]
        );
    }
}
