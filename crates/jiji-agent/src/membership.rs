//! Plain, single-writer mesh membership records.
//!
//! Membership is deliberately separate from service observations: only these
//! records may change WireGuard peers and routed subnets. Membership changes
//! originate solely from `jiji-cli`, computed locally from `jiji.yml` and
//! pushed directly over SSH to every reachable host -- there is no
//! peer-to-peer membership relay, so nothing needs a cryptographic signature
//! beyond "this file was installed by root," the same trust boundary every
//! other agent-managed file on the host already has.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const MEMBERSHIP_PROTOCOL_VERSION: u16 = 1;
pub const MEMBERSHIP_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MembershipState {
    Active,
    Tombstoned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MembershipRecord {
    pub project_id: String,
    pub recovery_epoch: u64,
    pub protocol_version: u16,
    pub schema_version: u16,
    pub node_id: String,
    pub server_name: String,
    pub wireguard_public_key: String,
    pub management_address: Ipv4Addr,
    pub container_subnet: String,
    pub endpoints: Vec<SocketAddr>,
    pub owner_epoch: u64,
    pub revision: u64,
    pub state: MembershipState,
}

impl MembershipRecord {
    pub fn validate(&self) -> Result<(), MembershipError> {
        if self.protocol_version != MEMBERSHIP_PROTOCOL_VERSION {
            return Err(MembershipError::ProtocolVersion(self.protocol_version));
        }
        if self.schema_version != MEMBERSHIP_SCHEMA_VERSION {
            return Err(MembershipError::SchemaVersion(self.schema_version));
        }
        if self.project_id.is_empty()
            || self.node_id.is_empty()
            || self.server_name.is_empty()
            || self.wireguard_public_key.trim().is_empty()
            || self.endpoints.is_empty()
            || self.revision == 0
            || self.owner_epoch == 0
        {
            return Err(MembershipError::InvalidRecord);
        }
        self.container_subnet
            .parse::<jiji_network::Ipv4Cidr>()
            .map_err(|_| MembershipError::InvalidSubnet)?;
        Ok(())
    }
}

/// Identifies the project/recovery-epoch a membership record must belong to.
/// Scoping used to be carried by the (now removed) `AuthorityKeyring`; it's a
/// plain value here because there's no signature to authenticate it against
/// -- each host's own `MeshConfig` already knows its `project_id`/
/// `recovery_epoch` locally.
#[derive(Debug, Clone)]
pub struct MembershipScope {
    project_id: String,
    recovery_epoch: u64,
}

impl MembershipScope {
    pub fn new(project_id: impl Into<String>, recovery_epoch: u64) -> Self {
        Self {
            project_id: project_id.into(),
            recovery_epoch,
        }
    }
}

/// A node's own project/epoch/node identity -- shared by everything that needs to say "this is
/// who I am": catalog/desired-state replication (`catalog_replication::ReplicationIdentity` used
/// to be a separate, field-for-field identical type) and the agent socket API's catalog/
/// desired-state writes (ditto for `api::CatalogIdentity`).
#[derive(Debug, Clone)]
pub struct NodeIdentity {
    pub project_id: String,
    pub recovery_epoch: u64,
    pub node_id: String,
}

impl NodeIdentity {
    pub fn scope(&self) -> MembershipScope {
        MembershipScope::new(self.project_id.clone(), self.recovery_epoch)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MembershipApply {
    Applied,
    Duplicate,
    Superseded,
}

#[derive(Debug, Clone, Default)]
pub struct MembershipView {
    records: BTreeMap<String, MembershipRecord>,
    record_ids: BTreeSet<String>,
}

impl MembershipView {
    pub fn from_records(
        records: impl IntoIterator<Item = MembershipRecord>,
        scope: &MembershipScope,
    ) -> Result<Self, MembershipError> {
        let mut view = Self::default();
        for record in records {
            view.apply(record, scope)?;
        }
        Ok(view)
    }

    pub fn apply(
        &mut self,
        record: MembershipRecord,
        scope: &MembershipScope,
    ) -> Result<MembershipApply, MembershipError> {
        record.validate()?;
        if record.project_id != scope.project_id {
            return Err(MembershipError::WrongProject);
        }
        if record.recovery_epoch != scope.recovery_epoch {
            return Err(MembershipError::RecoveryEpoch {
                expected: scope.recovery_epoch,
                actual: record.recovery_epoch,
            });
        }
        let id = record_id(&record)?;
        if self.record_ids.contains(&id) {
            return Ok(MembershipApply::Duplicate);
        }
        if let Some(current) = self.records.get(&record.node_id) {
            if record.owner_epoch < current.owner_epoch {
                return Err(MembershipError::StaleOwnerEpoch);
            }
            let current_id = record_id(current)?;
            if order(&record, &id) <= order(current, &current_id) {
                self.record_ids.insert(id);
                return Ok(MembershipApply::Superseded);
            }
            if current.state == MembershipState::Tombstoned
                && record.state == MembershipState::Active
                && record.owner_epoch == current.owner_epoch
            {
                return Err(MembershipError::TombstoneResurrection);
            }
        }
        if record.state == MembershipState::Active {
            self.reject_claim_collisions(&record)?;
        }
        self.record_ids.insert(id);
        self.records.insert(record.node_id.clone(), record);
        Ok(MembershipApply::Applied)
    }

    fn reject_claim_collisions(&self, candidate: &MembershipRecord) -> Result<(), MembershipError> {
        for current in self.active() {
            if current.node_id == candidate.node_id {
                continue;
            }
            if current.server_name == candidate.server_name {
                return Err(MembershipError::ServerNameClaimed);
            }
            if current.management_address == candidate.management_address {
                return Err(MembershipError::ManagementAddressClaimed);
            }
            if current.container_subnet == candidate.container_subnet {
                return Err(MembershipError::ContainerSubnetClaimed);
            }
            if current.wireguard_public_key == candidate.wireguard_public_key {
                return Err(MembershipError::WireGuardKeyClaimed);
            }
        }
        Ok(())
    }

    pub fn active(&self) -> impl Iterator<Item = &MembershipRecord> {
        self.records
            .values()
            .filter(|record| record.state == MembershipState::Active)
    }

    pub fn get(&self, node_id: &str) -> Option<&MembershipRecord> {
        self.records.get(node_id)
    }

    /// Every known member, including tombstones -- absence alone is never removal authority.
    pub fn all(&self) -> impl Iterator<Item = &MembershipRecord> {
        self.records.values()
    }

    /// Looks up a member by its management address, regardless of state --
    /// used to attribute an inbound catalog/desired-state connection (over
    /// its already WireGuard-authenticated source address) to the node_id it
    /// claims to own, without needing the caller to already know that
    /// node_id.
    pub fn find_by_management_address(&self, address: Ipv4Addr) -> Option<&MembershipRecord> {
        self.records
            .values()
            .find(|record| record.management_address == address)
    }

    /// Authenticates a claim of ownership by `claimed_owner`, given where the record came from.
    /// A `Local` record was constructed by this node itself just now and is trusted
    /// unconditionally. A `Verified` record is already durably persisted, previously
    /// authenticated at the time it was first ingested, and is only being replayed to
    /// reconstruct a view -- also trusted unconditionally, but for a different reason than
    /// `Local`, so it's a distinct variant rather than reusing one that means "I authored this."
    /// A `Peer` record arrived over a direct, WireGuard-mesh-only connection (never relayed
    /// through a third node, see `catalog_replication.rs`); its source address is unspoofable
    /// within the mesh, so resolving it back to a member's own `management_address` is sufficient
    /// to attribute the claim -- no signature is needed on top of that.
    pub fn authenticate(
        &self,
        provenance: RecordProvenance,
        claimed_owner: &str,
    ) -> Result<(), ProvenanceError> {
        match provenance {
            RecordProvenance::Local | RecordProvenance::Verified => Ok(()),
            RecordProvenance::Peer(source) => {
                let IpAddr::V4(source_ip) = source.ip() else {
                    return Err(ProvenanceError::UnknownPeer);
                };
                let Some(peer) = self.find_by_management_address(source_ip) else {
                    return Err(ProvenanceError::UnknownPeer);
                };
                if peer.node_id != claimed_owner {
                    return Err(ProvenanceError::NotOwner);
                }
                Ok(())
            }
        }
    }
}

/// Where a replicated record (catalog or desired-state) came from. See
/// `MembershipView::authenticate`.
#[derive(Debug, Clone, Copy)]
pub enum RecordProvenance {
    /// This node constructed the record itself, just now (the agent socket API, or local
    /// crash-restart reconciliation).
    Local,
    /// Already durably persisted and authenticated once at ingestion time; being replayed only to
    /// reconstruct a view, never itself the newly-applied record.
    Verified,
    Peer(SocketAddr),
}

#[derive(Debug, Error)]
pub enum ProvenanceError {
    #[error("record was not sent by a known member's own management address")]
    UnknownPeer,
    #[error("record claims ownership by a node other than the one that sent it")]
    NotOwner,
}

fn order(record: &MembershipRecord, id: &str) -> (u64, u64, u8, String) {
    (
        record.owner_epoch,
        record.revision,
        match record.state {
            MembershipState::Active => 0,
            MembershipState::Tombstoned => 1,
        },
        id.to_string(),
    )
}

pub(crate) fn record_id(record: &MembershipRecord) -> Result<String, MembershipError> {
    Ok(content_hash(record)?)
}

/// Deterministic content-hash id used for anti-entropy dedup (membership, catalog, and
/// desired-state records alike) -- purely an idempotency key, not a security boundary.
pub(crate) fn content_hash<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    let digest = Sha256::digest(serde_json::to_vec(value)?);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[derive(Debug, Error)]
pub enum MembershipError {
    #[error("membership serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("membership record is incomplete")]
    InvalidRecord,
    #[error("container subnet is invalid")]
    InvalidSubnet,
    #[error("membership protocol version {0} is unsupported")]
    ProtocolVersion(u16),
    #[error("membership schema version {0} is unsupported")]
    SchemaVersion(u16),
    #[error("membership record belongs to another project")]
    WrongProject,
    #[error("membership recovery epoch {actual} does not match {expected}")]
    RecoveryEpoch { expected: u64, actual: u64 },
    #[error("membership owner epoch moved backwards")]
    StaleOwnerEpoch,
    #[error("a tombstoned node cannot be resurrected in the same owner epoch")]
    TombstoneResurrection,
    #[error("server name is already claimed by an active node")]
    ServerNameClaimed,
    #[error("management address is already claimed by an active node")]
    ManagementAddressClaimed,
    #[error("container subnet is already claimed by an active node")]
    ContainerSubnetClaimed,
    #[error("WireGuard public key is already claimed by an active node")]
    WireGuardKeyClaimed,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> MembershipScope {
        MembershipScope::new("project-id", 1)
    }

    fn record(node: &str, address: u8, revision: u64) -> MembershipRecord {
        MembershipRecord {
            project_id: "project-id".into(),
            recovery_epoch: 1,
            protocol_version: MEMBERSHIP_PROTOCOL_VERSION,
            schema_version: MEMBERSHIP_SCHEMA_VERSION,
            node_id: node.into(),
            server_name: node.into(),
            wireguard_public_key: format!("wg-{node}"),
            management_address: Ipv4Addr::new(100, 98, 64, address),
            container_subnet: format!("198.18.{address}.0/24"),
            endpoints: vec![format!("192.0.2.{address}:51820").parse().unwrap()],
            owner_epoch: 1,
            revision,
            state: MembershipState::Active,
        }
    }

    #[test]
    fn project_and_recovery_epoch_scoping_are_enforced() {
        let scope = scope();
        let mut view = MembershipView::default();
        view.apply(record("a", 1, 1), &scope).unwrap();

        let mut wrong_project = record("b", 2, 1);
        wrong_project.project_id = "other-project".into();
        assert!(matches!(
            view.apply(wrong_project, &scope),
            Err(MembershipError::WrongProject)
        ));

        let mut wrong_epoch = record("b", 2, 1);
        wrong_epoch.recovery_epoch = 2;
        assert!(matches!(
            view.apply(wrong_epoch, &scope),
            Err(MembershipError::RecoveryEpoch { .. })
        ));
    }

    #[test]
    fn duplicate_out_of_order_and_tombstone_delivery_converge() {
        let scope = scope();
        let active = record("a", 1, 1);
        let newer = record("a", 1, 2);
        let mut tombstone = record("a", 1, 2);
        tombstone.state = MembershipState::Tombstoned;
        let mut view = MembershipView::default();
        assert_eq!(
            view.apply(newer.clone(), &scope).unwrap(),
            MembershipApply::Applied
        );
        assert_eq!(
            view.apply(active, &scope).unwrap(),
            MembershipApply::Superseded
        );
        assert_eq!(
            view.apply(tombstone.clone(), &scope).unwrap(),
            MembershipApply::Applied
        );
        assert_eq!(
            view.apply(tombstone, &scope).unwrap(),
            MembershipApply::Duplicate
        );
        assert!(view.active().next().is_none());
    }

    #[test]
    fn concurrent_claims_are_rejected() {
        let scope = scope();
        let mut view = MembershipView::default();
        view.apply(record("a", 1, 1), &scope).unwrap();
        let mut collision = record("b", 2, 1);
        collision.management_address = Ipv4Addr::new(100, 98, 64, 1);
        assert!(matches!(
            view.apply(collision, &scope),
            Err(MembershipError::ManagementAddressClaimed)
        ));
    }

    #[test]
    fn tombstone_requires_a_new_owner_epoch_to_replace() {
        let scope = scope();
        let mut tombstone = record("a", 1, 2);
        tombstone.state = MembershipState::Tombstoned;
        let mut view = MembershipView::default();
        view.apply(tombstone, &scope).unwrap();
        assert!(matches!(
            view.apply(record("a", 1, 3), &scope),
            Err(MembershipError::TombstoneResurrection)
        ));
        let mut replacement = record("a", 1, 1);
        replacement.owner_epoch = 2;
        view.apply(replacement, &scope).unwrap();
        assert_eq!(view.active().count(), 1);
    }

    #[test]
    fn a_stale_replay_cannot_resurrect_a_tombstoned_node() {
        let scope = scope();
        let active = record("node-a", 1, 1);
        let mut tombstone = record("node-a", 1, 2);
        tombstone.state = MembershipState::Tombstoned;
        let replay = record("node-a", 1, 3);
        let mut view = MembershipView::default();
        view.apply(active, &scope).unwrap();
        view.apply(tombstone, &scope).unwrap();
        assert!(matches!(
            view.apply(replay, &scope),
            Err(MembershipError::TombstoneResurrection)
        ));
    }

    #[test]
    fn a_newer_tombstone_in_the_same_owner_epoch_is_idempotent_fencing() {
        let scope = scope();
        let mut first = record("node-a", 1, 2);
        first.state = MembershipState::Tombstoned;
        let mut newer = first.clone();
        newer.revision = 3;
        let mut view = MembershipView::default();
        view.apply(first, &scope).unwrap();
        assert_eq!(view.apply(newer, &scope).unwrap(), MembershipApply::Applied);
    }
}
