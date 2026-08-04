//! Replicated, node-signed desired service placement.

use std::collections::{BTreeMap, BTreeSet};

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::membership::MembershipView;

pub const DESIRED_PROTOCOL_VERSION: u16 = 1;
pub const DESIRED_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplicaAssignment {
    pub replica_id: String,
    pub ordinal: u32,
    pub owner_node_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesiredStateRecord {
    pub project_id: String,
    pub recovery_epoch: u64,
    pub protocol_version: u16,
    pub schema_version: u16,
    pub service: String,
    /// `None` means reset to the configured replica count.
    pub replica_override: Option<u32>,
    pub assignments: Vec<ReplicaAssignment>,
    pub revision: u64,
    pub author_node_id: String,
    pub author_epoch: u64,
}

impl DesiredStateRecord {
    fn validate(&self) -> Result<(), DesiredError> {
        if self.protocol_version != DESIRED_PROTOCOL_VERSION
            || self.schema_version != DESIRED_SCHEMA_VERSION
        {
            return Err(DesiredError::IncompatibleVersion);
        }
        if self.project_id.is_empty()
            || self.service.is_empty()
            || self.author_node_id.is_empty()
            || self.revision == 0
            || self.author_epoch == 0
        {
            return Err(DesiredError::InvalidRecord);
        }
        let mut ids = BTreeSet::new();
        let mut ordinals = BTreeSet::new();
        if self.assignments.iter().any(|assignment| {
            assignment.replica_id.is_empty()
                || assignment.owner_node_id.is_empty()
                || !ids.insert(&assignment.replica_id)
                || !ordinals.insert(assignment.ordinal)
        }) {
            return Err(DesiredError::InvalidRecord);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedDesiredState {
    pub operation_id: String,
    pub signer_id: String,
    pub record: DesiredStateRecord,
    pub signature: Vec<u8>,
}

impl SignedDesiredState {
    pub fn sign(record: DesiredStateRecord, key: &SigningKey) -> Result<Self, DesiredError> {
        record.validate()?;
        let operation_id = operation_id(&record)?;
        Ok(Self {
            signature: key.sign(operation_id.as_bytes()).to_bytes().to_vec(),
            signer_id: record.author_node_id.clone(),
            operation_id,
            record,
        })
    }

    pub fn verify(
        &self,
        project_id: &str,
        recovery_epoch: u64,
        membership: &MembershipView,
    ) -> Result<(), DesiredError> {
        self.record.validate()?;
        if self.record.project_id != project_id {
            return Err(DesiredError::WrongProject);
        }
        if self.record.recovery_epoch != recovery_epoch {
            return Err(DesiredError::RecoveryEpoch);
        }
        if self.signer_id != self.record.author_node_id
            || operation_id(&self.record)? != self.operation_id
        {
            return Err(DesiredError::InvalidSignature);
        }
        let member = membership
            .get(&self.signer_id)
            .ok_or(DesiredError::UnknownSigner)?;
        if member.record.owner_epoch != self.record.author_epoch {
            return Err(DesiredError::StaleAuthor);
        }
        let bytes: [u8; 32] = member
            .record
            .node_signing_public_key
            .as_slice()
            .try_into()
            .map_err(|_| DesiredError::InvalidSignature)?;
        let key = ed25519_dalek::VerifyingKey::from_bytes(&bytes)
            .map_err(|_| DesiredError::InvalidSignature)?;
        let signature =
            Signature::from_slice(&self.signature).map_err(|_| DesiredError::InvalidSignature)?;
        key.verify(self.operation_id.as_bytes(), &signature)
            .map_err(|_| DesiredError::InvalidSignature)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesiredApply {
    Applied,
    Duplicate,
    Superseded,
}

#[derive(Default)]
pub struct DesiredStateView {
    records: BTreeMap<String, SignedDesiredState>,
    operation_ids: BTreeSet<String>,
}

impl DesiredStateView {
    pub fn from_operations(
        operations: impl IntoIterator<Item = SignedDesiredState>,
        project_id: &str,
        recovery_epoch: u64,
        membership: &MembershipView,
    ) -> Result<Self, DesiredError> {
        let mut view = Self::default();
        for operation in operations {
            view.apply(operation, project_id, recovery_epoch, membership)?;
        }
        Ok(view)
    }

    pub fn apply(
        &mut self,
        operation: SignedDesiredState,
        project_id: &str,
        recovery_epoch: u64,
        membership: &MembershipView,
    ) -> Result<DesiredApply, DesiredError> {
        operation.verify(project_id, recovery_epoch, membership)?;
        if !self.operation_ids.insert(operation.operation_id.clone()) {
            return Ok(DesiredApply::Duplicate);
        }
        if let Some(current) = self.records.get(&operation.record.service) {
            if order(&operation) <= order(current) {
                return Ok(DesiredApply::Superseded);
            }
        }
        self.records
            .insert(operation.record.service.clone(), operation);
        Ok(DesiredApply::Applied)
    }

    pub fn get(&self, service: &str) -> Option<&DesiredStateRecord> {
        self.records.get(service).map(|operation| &operation.record)
    }
}

fn order(operation: &SignedDesiredState) -> (u64, &str) {
    (operation.record.revision, &operation.operation_id)
}

fn operation_id(record: &DesiredStateRecord) -> Result<String, DesiredError> {
    let digest = Sha256::digest(serde_json::to_vec(record)?);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[derive(Debug, Error)]
pub enum DesiredError {
    #[error("desired state serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("desired state record is invalid")]
    InvalidRecord,
    #[error("desired state protocol or schema is incompatible")]
    IncompatibleVersion,
    #[error("desired state belongs to another project")]
    WrongProject,
    #[error("desired state belongs to another recovery epoch")]
    RecoveryEpoch,
    #[error("desired state signature is invalid")]
    InvalidSignature,
    #[error("desired state signer is not an active member")]
    UnknownSigner,
    #[error("desired state signer epoch is stale")]
    StaleAuthor,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::membership::{
        AuthorityKeyring, MembershipRecord, MembershipState, MembershipView, SignedMembership,
        MEMBERSHIP_PROTOCOL_VERSION, MEMBERSHIP_SCHEMA_VERSION,
    };

    fn fixture() -> (MembershipView, SigningKey) {
        let authority_key = SigningKey::from_bytes(&[3; 32]);
        let node_key = SigningKey::from_bytes(&[7; 32]);
        let mut authority = AuthorityKeyring::new("demo", 1);
        authority.add_authority("root", authority_key.verifying_key());
        let membership = SignedMembership::sign(
            MembershipRecord {
                project_id: "demo".into(),
                recovery_epoch: 1,
                protocol_version: MEMBERSHIP_PROTOCOL_VERSION,
                schema_version: MEMBERSHIP_SCHEMA_VERSION,
                node_id: "a".into(),
                server_name: "a".into(),
                node_signing_public_key: node_key.verifying_key().to_bytes().to_vec(),
                wireguard_public_key: "wg-a".into(),
                management_address: "100.98.0.1".parse().unwrap(),
                container_subnet: "198.18.0.0/24".into(),
                endpoints: vec!["192.0.2.1:51820".parse().unwrap()],
                owner_epoch: 1,
                revision: 1,
                state: MembershipState::Active,
            },
            "root",
            &authority_key,
        )
        .unwrap();
        let mut view = MembershipView::default();
        view.apply(membership, &authority).unwrap();
        (view, node_key)
    }

    fn operation(key: &SigningKey, revision: u64, replicas: u32) -> SignedDesiredState {
        SignedDesiredState::sign(
            DesiredStateRecord {
                project_id: "demo".into(),
                recovery_epoch: 1,
                protocol_version: DESIRED_PROTOCOL_VERSION,
                schema_version: DESIRED_SCHEMA_VERSION,
                service: "web".into(),
                replica_override: Some(replicas),
                assignments: (0..replicas)
                    .map(|ordinal| ReplicaAssignment {
                        replica_id: format!("web-{ordinal}"),
                        ordinal,
                        owner_node_id: "a".into(),
                    })
                    .collect(),
                revision,
                author_node_id: "a".into(),
                author_epoch: 1,
            },
            key,
        )
        .unwrap()
    }

    #[test]
    fn newer_signed_desired_state_wins_and_duplicates_are_idempotent() {
        let (membership, key) = fixture();
        let first = operation(&key, 1, 1);
        let second = operation(&key, 2, 2);
        let mut view = DesiredStateView::default();
        assert_eq!(
            view.apply(first.clone(), "demo", 1, &membership).unwrap(),
            DesiredApply::Applied
        );
        assert_eq!(
            view.apply(first, "demo", 1, &membership).unwrap(),
            DesiredApply::Duplicate
        );
        assert_eq!(
            view.apply(second, "demo", 1, &membership).unwrap(),
            DesiredApply::Applied
        );
        assert_eq!(view.get("web").unwrap().assignments.len(), 2);
    }

    #[test]
    fn forged_desired_state_is_rejected() {
        let (membership, _) = fixture();
        let forged = operation(&SigningKey::from_bytes(&[9; 32]), 1, 1);
        assert!(forged.verify("demo", 1, &membership).is_err());
    }
}
