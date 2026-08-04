//! Signed, single-authority mesh membership records.
//!
//! Membership is deliberately separate from service observations: only these
//! records may change WireGuard peers and routed subnets. Nodes relay signed
//! operations but cannot mint membership, reclaim an address, or resurrect a
//! tombstoned node.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{Ipv4Addr, SocketAddr};

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
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
    pub node_signing_public_key: Vec<u8>,
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
        if self.node_signing_public_key.len() != 32 {
            return Err(MembershipError::InvalidNodeSigningKey);
        }
        self.container_subnet
            .parse::<jiji_network::Ipv4Cidr>()
            .map_err(|_| MembershipError::InvalidSubnet)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedMembership {
    pub operation_id: String,
    pub signer_id: String,
    pub record: MembershipRecord,
    pub signature: Vec<u8>,
}

impl SignedMembership {
    pub fn sign(
        record: MembershipRecord,
        signer_id: impl Into<String>,
        signing_key: &SigningKey,
    ) -> Result<Self, MembershipError> {
        record.validate()?;
        let operation_id = operation_id(&record)?;
        Ok(Self {
            signature: signing_key
                .sign(operation_id.as_bytes())
                .to_bytes()
                .to_vec(),
            operation_id,
            signer_id: signer_id.into(),
            record,
        })
    }

    pub fn verify(&self, authority: &AuthorityKeyring) -> Result<(), MembershipError> {
        self.record.validate()?;
        if self.record.project_id != authority.project_id {
            return Err(MembershipError::WrongProject);
        }
        if self.record.recovery_epoch != authority.recovery_epoch {
            return Err(MembershipError::RecoveryEpoch {
                expected: authority.recovery_epoch,
                actual: self.record.recovery_epoch,
            });
        }
        if operation_id(&self.record)? != self.operation_id {
            return Err(MembershipError::InvalidOperationId);
        }
        let signature = Signature::from_slice(&self.signature)
            .map_err(|_| MembershipError::InvalidSignature)?;
        let Some(keys) = authority.authorities.get(&self.signer_id) else {
            return Err(MembershipError::UnknownAuthority);
        };
        if !keys
            .iter()
            .any(|key| key.verify(self.operation_id.as_bytes(), &signature).is_ok())
        {
            return Err(MembershipError::InvalidSignature);
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct AuthorityKeyring {
    project_id: String,
    recovery_epoch: u64,
    authorities: BTreeMap<String, Vec<VerifyingKey>>,
}

impl AuthorityKeyring {
    pub fn new(project_id: impl Into<String>, recovery_epoch: u64) -> Self {
        Self {
            project_id: project_id.into(),
            recovery_epoch,
            authorities: BTreeMap::new(),
        }
    }

    pub fn add_authority(&mut self, id: impl Into<String>, key: VerifyingKey) {
        self.authorities.entry(id.into()).or_default().push(key);
    }

    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    pub fn recovery_epoch(&self) -> u64 {
        self.recovery_epoch
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
    records: BTreeMap<String, SignedMembership>,
    operation_ids: BTreeSet<String>,
}

impl MembershipView {
    pub fn from_operations(
        operations: impl IntoIterator<Item = SignedMembership>,
        authority: &AuthorityKeyring,
    ) -> Result<Self, MembershipError> {
        let mut view = Self::default();
        for operation in operations {
            view.apply(operation, authority)?;
        }
        Ok(view)
    }

    pub fn apply(
        &mut self,
        operation: SignedMembership,
        authority: &AuthorityKeyring,
    ) -> Result<MembershipApply, MembershipError> {
        operation.verify(authority)?;
        if self.operation_ids.contains(&operation.operation_id) {
            return Ok(MembershipApply::Duplicate);
        }
        if let Some(current) = self.records.get(&operation.record.node_id) {
            if operation.record.owner_epoch < current.record.owner_epoch {
                return Err(MembershipError::StaleOwnerEpoch);
            }
            if order(&operation) <= order(current) {
                self.operation_ids.insert(operation.operation_id);
                return Ok(MembershipApply::Superseded);
            }
            if current.record.state == MembershipState::Tombstoned
                && operation.record.state == MembershipState::Active
                && operation.record.owner_epoch == current.record.owner_epoch
            {
                return Err(MembershipError::TombstoneResurrection);
            }
        }
        if operation.record.state == MembershipState::Active {
            self.reject_claim_collisions(&operation)?;
        }
        self.operation_ids.insert(operation.operation_id.clone());
        self.records
            .insert(operation.record.node_id.clone(), operation);
        Ok(MembershipApply::Applied)
    }

    fn reject_claim_collisions(&self, candidate: &SignedMembership) -> Result<(), MembershipError> {
        for current in self.active() {
            if current.record.node_id == candidate.record.node_id {
                continue;
            }
            if current.record.server_name == candidate.record.server_name {
                return Err(MembershipError::ServerNameClaimed);
            }
            if current.record.management_address == candidate.record.management_address {
                return Err(MembershipError::ManagementAddressClaimed);
            }
            if current.record.container_subnet == candidate.record.container_subnet {
                return Err(MembershipError::ContainerSubnetClaimed);
            }
            if current.record.wireguard_public_key == candidate.record.wireguard_public_key {
                return Err(MembershipError::WireGuardKeyClaimed);
            }
        }
        Ok(())
    }

    pub fn active(&self) -> impl Iterator<Item = &SignedMembership> {
        self.records
            .values()
            .filter(|entry| entry.record.state == MembershipState::Active)
    }

    pub fn get(&self, node_id: &str) -> Option<&SignedMembership> {
        self.records.get(node_id)
    }
}

fn order(operation: &SignedMembership) -> (u64, u64, u8, &str) {
    (
        operation.record.owner_epoch,
        operation.record.revision,
        match operation.record.state {
            MembershipState::Active => 0,
            MembershipState::Tombstoned => 1,
        },
        &operation.operation_id,
    )
}

fn operation_id(record: &MembershipRecord) -> Result<String, MembershipError> {
    let digest = Sha256::digest(serde_json::to_vec(record)?);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[derive(Debug, Error)]
pub enum MembershipError {
    #[error("membership serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("membership record is incomplete")]
    InvalidRecord,
    #[error("node signing public key is invalid")]
    InvalidNodeSigningKey,
    #[error("container subnet is invalid")]
    InvalidSubnet,
    #[error("membership protocol version {0} is unsupported")]
    ProtocolVersion(u16),
    #[error("membership schema version {0} is unsupported")]
    SchemaVersion(u16),
    #[error("membership operation belongs to another project")]
    WrongProject,
    #[error("membership recovery epoch {actual} does not match {expected}")]
    RecoveryEpoch { expected: u64, actual: u64 },
    #[error("membership operation ID does not match its body")]
    InvalidOperationId,
    #[error("membership operation has an invalid signature")]
    InvalidSignature,
    #[error("membership operation was not signed by a trusted project authority")]
    UnknownAuthority,
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

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn authority() -> (AuthorityKeyring, SigningKey) {
        let key = key(1);
        let mut authority = AuthorityKeyring::new("project-id", 1);
        authority.add_authority("root", key.verifying_key());
        (authority, key)
    }

    fn record(node: &str, address: u8, revision: u64) -> MembershipRecord {
        MembershipRecord {
            project_id: "project-id".into(),
            recovery_epoch: 1,
            protocol_version: MEMBERSHIP_PROTOCOL_VERSION,
            schema_version: MEMBERSHIP_SCHEMA_VERSION,
            node_id: node.into(),
            server_name: node.into(),
            node_signing_public_key: vec![address; 32],
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
    fn authority_signature_and_project_epoch_are_required() {
        let (authority, signing_key) = authority();
        let signed = SignedMembership::sign(record("a", 1, 1), "root", &signing_key).unwrap();
        signed.verify(&authority).unwrap();

        let forged = SignedMembership::sign(record("b", 2, 1), "root", &key(9)).unwrap();
        assert!(matches!(
            forged.verify(&authority),
            Err(MembershipError::InvalidSignature)
        ));
        let mut wrong_epoch = record("b", 2, 1);
        wrong_epoch.recovery_epoch = 2;
        let wrong_epoch = SignedMembership::sign(wrong_epoch, "root", &signing_key).unwrap();
        assert!(matches!(
            wrong_epoch.verify(&authority),
            Err(MembershipError::RecoveryEpoch { .. })
        ));
    }

    #[test]
    fn duplicate_out_of_order_and_tombstone_delivery_converge() {
        let (authority, key) = authority();
        let active = SignedMembership::sign(record("a", 1, 1), "root", &key).unwrap();
        let newer = SignedMembership::sign(record("a", 1, 2), "root", &key).unwrap();
        let mut tombstone_record = record("a", 1, 2);
        tombstone_record.state = MembershipState::Tombstoned;
        let tombstone = SignedMembership::sign(tombstone_record, "root", &key).unwrap();
        let mut view = MembershipView::default();
        assert_eq!(
            view.apply(newer, &authority).unwrap(),
            MembershipApply::Applied
        );
        assert_eq!(
            view.apply(active, &authority).unwrap(),
            MembershipApply::Superseded
        );
        assert_eq!(
            view.apply(tombstone.clone(), &authority).unwrap(),
            MembershipApply::Applied
        );
        assert_eq!(
            view.apply(tombstone, &authority).unwrap(),
            MembershipApply::Duplicate
        );
        assert!(view.active().next().is_none());
    }

    #[test]
    fn concurrent_claims_are_rejected() {
        let (authority, key) = authority();
        let mut view = MembershipView::default();
        view.apply(
            SignedMembership::sign(record("a", 1, 1), "root", &key).unwrap(),
            &authority,
        )
        .unwrap();
        let mut collision = record("b", 2, 1);
        collision.management_address = Ipv4Addr::new(100, 98, 64, 1);
        assert!(matches!(
            view.apply(
                SignedMembership::sign(collision, "root", &key).unwrap(),
                &authority
            ),
            Err(MembershipError::ManagementAddressClaimed)
        ));
    }

    #[test]
    fn tombstone_requires_a_new_owner_epoch_to_replace() {
        let (authority, key) = authority();
        let mut tombstone = record("a", 1, 2);
        tombstone.state = MembershipState::Tombstoned;
        let mut view = MembershipView::default();
        view.apply(
            SignedMembership::sign(tombstone, "root", &key).unwrap(),
            &authority,
        )
        .unwrap();
        assert!(matches!(
            view.apply(
                SignedMembership::sign(record("a", 1, 3), "root", &key).unwrap(),
                &authority
            ),
            Err(MembershipError::TombstoneResurrection)
        ));
        let mut replacement = record("a", 1, 1);
        replacement.owner_epoch = 2;
        view.apply(
            SignedMembership::sign(replacement, "root", &key).unwrap(),
            &authority,
        )
        .unwrap();
        assert_eq!(view.active().count(), 1);
    }

    #[test]
    fn compromised_node_cannot_republish_after_authority_tombstone() {
        let (authority, key) = authority();
        let active = SignedMembership::sign(record("node-a", 1, 1), "root", &key).unwrap();
        let mut tombstone_record = record("node-a", 1, 2);
        tombstone_record.state = MembershipState::Tombstoned;
        let tombstone = SignedMembership::sign(tombstone_record, "root", &key).unwrap();
        let replay = SignedMembership::sign(record("node-a", 1, 3), "root", &key).unwrap();
        let mut view = MembershipView::default();
        view.apply(active, &authority).unwrap();
        view.apply(tombstone, &authority).unwrap();
        assert!(matches!(
            view.apply(replay, &authority),
            Err(MembershipError::TombstoneResurrection)
        ));
    }

    #[test]
    fn a_newer_tombstone_in_the_same_owner_epoch_is_idempotent_fencing() {
        let (authority, key) = authority();
        let mut first = record("node-a", 1, 2);
        first.state = MembershipState::Tombstoned;
        let mut newer = first.clone();
        newer.revision = 3;
        let mut view = MembershipView::default();
        view.apply(
            SignedMembership::sign(first, "root", &key).unwrap(),
            &authority,
        )
        .unwrap();
        assert_eq!(
            view.apply(
                SignedMembership::sign(newer, "root", &key).unwrap(),
                &authority
            )
            .unwrap(),
            MembershipApply::Applied
        );
    }

    #[test]
    fn authority_rotation_overlap_accepts_both_then_old_key_can_be_retired() {
        let old = SigningKey::from_bytes(&[2; 32]);
        let new = SigningKey::from_bytes(&[3; 32]);
        let mut overlap = AuthorityKeyring::new("project-id", 1);
        overlap.add_authority("root", old.verifying_key());
        overlap.add_authority("root", new.verifying_key());
        let old_operation = SignedMembership::sign(record("node-a", 1, 1), "root", &old).unwrap();
        let new_operation = SignedMembership::sign(record("node-a", 1, 2), "root", &new).unwrap();
        assert!(old_operation.verify(&overlap).is_ok());
        assert!(new_operation.verify(&overlap).is_ok());

        let mut retired = AuthorityKeyring::new("project-id", 1);
        retired.add_authority("root", new.verifying_key());
        assert!(matches!(
            old_operation.verify(&retired),
            Err(MembershipError::InvalidSignature)
        ));
        assert!(new_operation.verify(&retired).is_ok());
    }
}
