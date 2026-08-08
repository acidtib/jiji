//! Plain, node-owned service catalog records.
//!
//! Unlike membership (CLI-pushed, see `membership.rs`), a catalog record is
//! *authored* by the node that currently owns the replica it describes -- a
//! node writes its own heartbeat and owns the logical service replicas
//! scheduled to it. There is no signing authority: authenticity comes from
//! `RecordProvenance` instead of a signature. A `Local` record was
//! constructed by this node itself (via the agent socket API or its own
//! crash-restart reconciliation) and is trusted unconditionally. A
//! `Verified` record is already durably persisted and was authenticated once
//! already, at ingestion time -- it's only being replayed to reconstruct a
//! view (rebuilding history before applying a new record, or serving DNS/
//! diagnostics reads), never itself the record being newly applied. A `Peer`
//! record arrived over `catalog_replication`'s direct, WireGuard-mesh-only
//! TCP connection; its claimed `owner_node_id` must match the node whose
//! membership record's `management_address` equals the connection's actual
//! source address -- since replication only ever sends a node's own records
//! (never relayed third-party state, see `catalog_replication.rs`), that
//! source address is exactly the node vouching for the claim, and WireGuard's
//! own peer authentication makes that address unspoofable within the mesh.
//!
//! Catalog records are written by `jiji-cli`'s deploy/restart/rollback/remove/scale commands (via
//! the agent's `CatalogCommit` API) and by this node's own crash-restart reconciliation
//! (`local_reconcile.rs`, gated on `jiji.catalog-managed=true`); there is no separate background
//! process that adopts arbitrary running containers into the catalog on its own initiative.

use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::membership::{MembershipView, ProvenanceError, RecordProvenance};

pub const CATALOG_PROTOCOL_VERSION: u16 = 1;
pub const CATALOG_SCHEMA_VERSION: u16 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentState {
    Candidate,
    Active,
    Draining,
    Stopped,
    Tombstoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    Unknown,
    Healthy,
    Unhealthy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogRecord {
    pub project_id: String,
    pub recovery_epoch: u64,
    pub protocol_version: u16,
    pub schema_version: u16,
    pub service: String,
    /// Stable across redeploys; survives restart and points at exactly one active deployment
    /// (see the plan's "Service catalog" section). Deterministic per `(project, service, ordinal)`
    /// via `jiji-cli`'s `placement::endpoint_replica_id`, independent of which host currently owns
    /// it.
    pub replica_id: String,
    pub owner_node_id: String,
    pub owner_epoch: u64,
    pub revision: u64,
    pub deployment_id: String,
    pub address: Ipv4Addr,
    pub ports: Vec<u16>,
    pub image: String,
    pub state: DeploymentState,
    pub health: HealthState,
}

impl CatalogRecord {
    pub fn validate(&self) -> Result<(), CatalogError> {
        if self.protocol_version != CATALOG_PROTOCOL_VERSION {
            return Err(CatalogError::ProtocolVersion(self.protocol_version));
        }
        if self.schema_version != CATALOG_SCHEMA_VERSION {
            return Err(CatalogError::SchemaVersion(self.schema_version));
        }
        if self.project_id.is_empty()
            || self.service.is_empty()
            || self.replica_id.is_empty()
            || self.owner_node_id.is_empty()
            || self.deployment_id.is_empty()
            || self.revision == 0
            || self.owner_epoch == 0
        {
            return Err(CatalogError::InvalidRecord);
        }
        Ok(())
    }
}

impl From<ProvenanceError> for CatalogError {
    fn from(error: ProvenanceError) -> Self {
        match error {
            ProvenanceError::UnknownPeer => CatalogError::UnknownPeer,
            ProvenanceError::NotOwner => CatalogError::NotOwner,
        }
    }
}

fn verify(
    record: &CatalogRecord,
    provenance: RecordProvenance,
    project_id: &str,
    recovery_epoch: u64,
    membership: &MembershipView,
) -> Result<(), CatalogError> {
    record.validate()?;
    if record.project_id != project_id {
        return Err(CatalogError::WrongProject);
    }
    if record.recovery_epoch != recovery_epoch {
        return Err(CatalogError::RecoveryEpoch {
            expected: recovery_epoch,
            actual: record.recovery_epoch,
        });
    }
    membership.authenticate(provenance, &record.owner_node_id)?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogApply {
    Applied,
    Duplicate,
    Superseded,
}

#[derive(Debug, Clone, Default)]
pub struct CatalogView {
    records: BTreeMap<String, CatalogRecord>,
    record_ids: BTreeSet<String>,
}

impl CatalogView {
    pub fn from_records(
        records: impl IntoIterator<Item = (CatalogRecord, RecordProvenance)>,
        project_id: &str,
        recovery_epoch: u64,
        membership: &MembershipView,
    ) -> Result<Self, CatalogError> {
        let mut view = Self::default();
        for (record, provenance) in records {
            view.apply(record, provenance, project_id, recovery_epoch, membership)?;
        }
        Ok(view)
    }

    pub fn apply(
        &mut self,
        record: CatalogRecord,
        provenance: RecordProvenance,
        project_id: &str,
        recovery_epoch: u64,
        membership: &MembershipView,
    ) -> Result<CatalogApply, CatalogError> {
        verify(&record, provenance, project_id, recovery_epoch, membership)?;
        let id = record_id(&record)?;
        if self.record_ids.contains(&id) {
            return Ok(CatalogApply::Duplicate);
        }
        let replica_owner_epoch = self
            .records
            .values()
            .filter(|current| current.replica_id == record.replica_id)
            .map(|current| current.owner_epoch)
            .max();
        if replica_owner_epoch.is_some_and(|epoch| record.owner_epoch < epoch) {
            return Err(CatalogError::StaleOwnerEpoch);
        }
        if let Some(current) = self.records.get(&record.deployment_id) {
            if record.owner_epoch < current.owner_epoch {
                return Err(CatalogError::StaleOwnerEpoch);
            }
            let current_id = record_id(current)?;
            if order(&record, &id) <= order(current, &current_id) {
                self.record_ids.insert(id);
                return Ok(CatalogApply::Superseded);
            }
            if current.state == DeploymentState::Tombstoned
                && record.state != DeploymentState::Tombstoned
                && record.owner_epoch == current.owner_epoch
            {
                return Err(CatalogError::TombstoneResurrection);
            }
        }
        self.record_ids.insert(id);
        self.records.insert(record.deployment_id.clone(), record);
        Ok(CatalogApply::Applied)
    }

    /// Active, healthy replicas -- the only records DNS may ever answer with. Candidate,
    /// draining, stopped, and tombstoned records are excluded.
    pub fn active_healthy(&self) -> impl Iterator<Item = &CatalogRecord> {
        let mut winners = BTreeMap::<&str, &CatalogRecord>::new();
        for record in self.records.values().filter(|record| {
            record.state == DeploymentState::Active && record.health == HealthState::Healthy
        }) {
            let replace = winners
                .get(record.replica_id.as_str())
                .is_none_or(|current| record_order(record) > record_order(current));
            if replace {
                winners.insert(record.replica_id.as_str(), record);
            }
        }
        winners.into_values()
    }

    pub fn get(&self, replica_id: &str) -> Option<&CatalogRecord> {
        self.records
            .values()
            .filter(|record| record.replica_id == replica_id)
            .max_by_key(|record| record_order(record))
    }

    pub fn all(&self) -> impl Iterator<Item = &CatalogRecord> {
        self.records.values()
    }
}

pub fn active_healthy_winners(records: &[CatalogRecord]) -> Vec<&CatalogRecord> {
    let mut winners = BTreeMap::<&str, &CatalogRecord>::new();
    for record in records.iter().filter(|record| {
        record.state == DeploymentState::Active && record.health == HealthState::Healthy
    }) {
        if winners
            .get(record.replica_id.as_str())
            .is_none_or(|current| record_order(record) > record_order(current))
        {
            winners.insert(record.replica_id.as_str(), record);
        }
    }
    winners.into_values().collect()
}

fn record_order(record: &CatalogRecord) -> (u64, u64, u8, &str) {
    (
        record.owner_epoch,
        record.revision,
        u8::from(record.state == DeploymentState::Tombstoned),
        record.deployment_id.as_str(),
    )
}

fn order(record: &CatalogRecord, id: &str) -> (u64, u64, u8, String) {
    (
        record.owner_epoch,
        record.revision,
        match record.state {
            DeploymentState::Tombstoned => 1,
            _ => 0,
        },
        id.to_string(),
    )
}

pub(crate) fn record_id(record: &CatalogRecord) -> Result<String, CatalogError> {
    Ok(crate::membership::content_hash(record)?)
}

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("catalog serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("catalog record is incomplete")]
    InvalidRecord,
    #[error("catalog protocol version {0} is unsupported")]
    ProtocolVersion(u16),
    #[error("catalog schema version {0} is unsupported")]
    SchemaVersion(u16),
    #[error("catalog operation belongs to another project")]
    WrongProject,
    #[error("catalog recovery epoch {actual} does not match {expected}")]
    RecoveryEpoch { expected: u64, actual: u64 },
    #[error("catalog operation was not sent by a known member's own management address")]
    UnknownPeer,
    #[error("a node may only publish catalog state it owns")]
    NotOwner,
    #[error("catalog owner epoch moved backwards")]
    StaleOwnerEpoch,
    #[error("a tombstoned replica cannot be resurrected in the same owner epoch")]
    TombstoneResurrection,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::membership::{
        MembershipRecord, MembershipScope, MembershipState, MEMBERSHIP_PROTOCOL_VERSION,
        MEMBERSHIP_SCHEMA_VERSION,
    };
    use std::net::{IpAddr, SocketAddr};

    fn membership_with(node_id: &str, management_address: Ipv4Addr) -> MembershipView {
        let scope = MembershipScope::new("project", 1);
        let record = MembershipRecord {
            project_id: "project".into(),
            recovery_epoch: 1,
            protocol_version: MEMBERSHIP_PROTOCOL_VERSION,
            schema_version: MEMBERSHIP_SCHEMA_VERSION,
            node_id: node_id.into(),
            server_name: node_id.into(),
            wireguard_public_key: format!("wg-{node_id}"),
            management_address,
            container_subnet: "198.18.1.0/24".into(),
            endpoints: vec!["192.0.2.1:51820".parse::<SocketAddr>().unwrap()],
            owner_epoch: 1,
            revision: 1,
            state: MembershipState::Active,
        };
        let mut view = MembershipView::default();
        view.apply(record, &scope).unwrap();
        view
    }

    fn peer_addr(management_address: Ipv4Addr) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(management_address), 58000)
    }

    fn record(replica: &str, node: &str, revision: u64, state: DeploymentState) -> CatalogRecord {
        CatalogRecord {
            project_id: "project".into(),
            recovery_epoch: 1,
            protocol_version: CATALOG_PROTOCOL_VERSION,
            schema_version: CATALOG_SCHEMA_VERSION,
            service: "web".into(),
            replica_id: replica.into(),
            owner_node_id: node.into(),
            owner_epoch: 1,
            revision,
            deployment_id: format!("deploy-{replica}"),
            address: "198.18.1.10".parse().unwrap(),
            ports: vec![80],
            image: "nginx:alpine".into(),
            state,
            health: HealthState::Healthy,
        }
    }

    #[test]
    fn a_node_can_publish_its_own_replica_locally() {
        let membership = membership_with("node-a", Ipv4Addr::new(100, 98, 64, 1));
        let mut view = CatalogView::default();
        view.apply(
            record("r1", "node-a", 1, DeploymentState::Active),
            RecordProvenance::Local,
            "project",
            1,
            &membership,
        )
        .unwrap();
    }

    #[test]
    fn a_peer_record_is_accepted_when_the_source_address_matches_the_owner() {
        let address = Ipv4Addr::new(100, 98, 64, 1);
        let membership = membership_with("node-a", address);
        let mut view = CatalogView::default();
        view.apply(
            record("r1", "node-a", 1, DeploymentState::Active),
            RecordProvenance::Peer(peer_addr(address)),
            "project",
            1,
            &membership,
        )
        .unwrap();
    }

    #[test]
    fn a_peer_cannot_claim_ownership_of_another_nodes_replica() {
        // node-a's own connection (source address matches node-a's membership record) relays a
        // record that claims ownership by node-b -- without the address-vs-owner check this
        // would otherwise be accepted as long as node-b is a known member.
        let node_a_address = Ipv4Addr::new(100, 98, 64, 1);
        let mut membership = membership_with("node-a", node_a_address);
        let scope = MembershipScope::new("project", 1);
        membership
            .apply(
                MembershipRecord {
                    project_id: "project".into(),
                    recovery_epoch: 1,
                    protocol_version: MEMBERSHIP_PROTOCOL_VERSION,
                    schema_version: MEMBERSHIP_SCHEMA_VERSION,
                    node_id: "node-b".into(),
                    server_name: "node-b".into(),
                    wireguard_public_key: "wg-node-b".into(),
                    management_address: Ipv4Addr::new(100, 98, 64, 2),
                    container_subnet: "198.18.2.0/24".into(),
                    endpoints: vec!["192.0.2.2:51820".parse::<SocketAddr>().unwrap()],
                    owner_epoch: 1,
                    revision: 1,
                    state: MembershipState::Active,
                },
                &scope,
            )
            .unwrap();
        let forged = record("r1", "node-b", 1, DeploymentState::Active);
        let mut view = CatalogView::default();
        assert!(matches!(
            view.apply(
                forged,
                RecordProvenance::Peer(peer_addr(node_a_address)),
                "project",
                1,
                &membership
            ),
            Err(CatalogError::NotOwner)
        ));
    }

    #[test]
    fn a_peer_record_from_an_unrecognized_address_is_rejected() {
        let membership = membership_with("node-a", Ipv4Addr::new(100, 98, 64, 1));
        let signed = record("r1", "node-a", 1, DeploymentState::Active);
        let mut view = CatalogView::default();
        assert!(matches!(
            view.apply(
                signed,
                RecordProvenance::Peer(peer_addr(Ipv4Addr::new(100, 98, 64, 9))),
                "project",
                1,
                &membership
            ),
            Err(CatalogError::UnknownPeer)
        ));
    }

    #[test]
    fn duplicate_and_out_of_order_delivery_converge() {
        let membership = membership_with("node-a", Ipv4Addr::new(100, 98, 64, 1));
        let older = record("r1", "node-a", 1, DeploymentState::Candidate);
        let newer = record("r1", "node-a", 2, DeploymentState::Active);
        let mut view = CatalogView::default();
        assert_eq!(
            view.apply(
                newer.clone(),
                RecordProvenance::Local,
                "project",
                1,
                &membership
            )
            .unwrap(),
            CatalogApply::Applied
        );
        assert_eq!(
            view.apply(older, RecordProvenance::Local, "project", 1, &membership)
                .unwrap(),
            CatalogApply::Superseded
        );
        assert_eq!(
            view.apply(newer, RecordProvenance::Local, "project", 1, &membership)
                .unwrap(),
            CatalogApply::Duplicate
        );
        assert_eq!(view.get("r1").unwrap().state, DeploymentState::Active);
    }

    #[test]
    fn candidate_and_draining_records_are_excluded_from_active_healthy() {
        let membership = membership_with("node-a", Ipv4Addr::new(100, 98, 64, 1));
        let mut view = CatalogView::default();
        for (id, state) in [
            ("r-candidate", DeploymentState::Candidate),
            ("r-active", DeploymentState::Active),
            ("r-draining", DeploymentState::Draining),
            ("r-stopped", DeploymentState::Stopped),
        ] {
            view.apply(
                record(id, "node-a", 1, state),
                RecordProvenance::Local,
                "project",
                1,
                &membership,
            )
            .unwrap();
        }
        let active: Vec<&str> = view
            .active_healthy()
            .map(|record| record.replica_id.as_str())
            .collect();
        assert_eq!(active, vec!["r-active"]);
    }

    #[test]
    fn one_logical_replica_publishes_only_its_newest_active_deployment() {
        let mut old = record("r1", "node-a", 2, DeploymentState::Active);
        old.deployment_id = "deploy-old".into();
        old.address = "198.18.1.10".parse().unwrap();
        let mut new = record("r1", "node-a", 4, DeploymentState::Active);
        new.deployment_id = "deploy-new".into();
        new.address = "198.18.1.11".parse().unwrap();
        let records = [old, new];
        let winners = active_healthy_winners(&records);
        assert_eq!(winners.len(), 1);
        assert_eq!(winners[0].deployment_id, "deploy-new");
        assert_eq!(
            winners[0].address,
            "198.18.1.11".parse::<Ipv4Addr>().unwrap()
        );
    }

    #[test]
    fn tombstone_requires_a_new_owner_epoch_to_replace() {
        let membership = membership_with("node-a", Ipv4Addr::new(100, 98, 64, 1));
        let mut view = CatalogView::default();
        let mut tombstone = record("r1", "node-a", 2, DeploymentState::Tombstoned);
        tombstone.owner_epoch = 1;
        view.apply(
            tombstone,
            RecordProvenance::Local,
            "project",
            1,
            &membership,
        )
        .unwrap();
        let replay = record("r1", "node-a", 3, DeploymentState::Active);
        assert!(matches!(
            view.apply(replay, RecordProvenance::Local, "project", 1, &membership),
            Err(CatalogError::TombstoneResurrection)
        ));
        let mut replacement = record("r1", "node-a", 1, DeploymentState::Active);
        replacement.owner_epoch = 2;
        view.apply(
            replacement,
            RecordProvenance::Local,
            "project",
            1,
            &membership,
        )
        .unwrap();
        assert_eq!(view.active_healthy().count(), 1);
    }

    #[test]
    fn wrong_project_and_recovery_epoch_are_rejected() {
        let membership = membership_with("node-a", Ipv4Addr::new(100, 98, 64, 1));
        let signed = record("r1", "node-a", 1, DeploymentState::Active);
        let mut view = CatalogView::default();
        assert!(matches!(
            view.apply(
                signed.clone(),
                RecordProvenance::Local,
                "other-project",
                1,
                &membership
            ),
            Err(CatalogError::WrongProject)
        ));
        assert!(matches!(
            view.apply(signed, RecordProvenance::Local, "project", 2, &membership),
            Err(CatalogError::RecoveryEpoch { .. })
        ));
    }
}
