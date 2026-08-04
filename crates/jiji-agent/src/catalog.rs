//! Signed, node-owned service catalog records.
//!
//! Unlike membership (authority-signed, see `membership.rs`), a catalog record is signed by the
//! node that currently owns the replica it describes -- a node writes its own heartbeat and owns
//! the logical service replicas scheduled to it. There is no separate catalog signing authority:
//! verification checks the signature against the signer's own `node_signing_public_key`, sourced
//! from the live membership view, and rejects any operation whose `signer_id` is not also its
//! `owner_node_id` -- a node can publish catalog state for itself, never on behalf of another
//! node.
//!
//! Catalog records are written by `jiji-cli`'s deploy/restart/rollback/remove/scale commands (via
//! the agent's `CatalogCommit` API) and by this node's own crash-restart reconciliation
//! (`local_reconcile.rs`, gated on `jiji.catalog-managed=true`); there is no separate background
//! process that adopts arbitrary running containers into the catalog on its own initiative.

use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::membership::MembershipView;

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedCatalogOperation {
    pub operation_id: String,
    pub signer_id: String,
    pub record: CatalogRecord,
    pub signature: Vec<u8>,
}

impl SignedCatalogOperation {
    pub fn sign(record: CatalogRecord, signing_key: &SigningKey) -> Result<Self, CatalogError> {
        record.validate()?;
        let operation_id = operation_id(&record)?;
        Ok(Self {
            signature: signing_key
                .sign(operation_id.as_bytes())
                .to_bytes()
                .to_vec(),
            operation_id,
            signer_id: record.owner_node_id.clone(),
            record,
        })
    }

    /// Verifies structure, project/epoch scoping, and the signature against the signer's own
    /// `node_signing_public_key` as published in `membership` -- a node with no active membership
    /// record cannot publish catalog state, and a node cannot sign a record it does not own.
    pub fn verify(
        &self,
        project_id: &str,
        recovery_epoch: u64,
        membership: &MembershipView,
    ) -> Result<(), CatalogError> {
        self.record.validate()?;
        if self.record.project_id != project_id {
            return Err(CatalogError::WrongProject);
        }
        if self.record.recovery_epoch != recovery_epoch {
            return Err(CatalogError::RecoveryEpoch {
                expected: recovery_epoch,
                actual: self.record.recovery_epoch,
            });
        }
        if self.signer_id != self.record.owner_node_id {
            return Err(CatalogError::NotOwner);
        }
        if operation_id(&self.record)? != self.operation_id {
            return Err(CatalogError::InvalidOperationId);
        }
        let signature =
            Signature::from_slice(&self.signature).map_err(|_| CatalogError::InvalidSignature)?;
        let Some(signer) = membership.get(&self.signer_id) else {
            return Err(CatalogError::UnknownSigner);
        };
        let key_bytes: [u8; 32] = signer
            .record
            .node_signing_public_key
            .as_slice()
            .try_into()
            .map_err(|_| CatalogError::InvalidSignature)?;
        let verifying_key = ed25519_dalek::VerifyingKey::from_bytes(&key_bytes)
            .map_err(|_| CatalogError::InvalidSignature)?;
        verifying_key
            .verify(self.operation_id.as_bytes(), &signature)
            .map_err(|_| CatalogError::InvalidSignature)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogApply {
    Applied,
    Duplicate,
    Superseded,
}

#[derive(Debug, Clone, Default)]
pub struct CatalogView {
    records: BTreeMap<String, SignedCatalogOperation>,
    operation_ids: BTreeSet<String>,
}

impl CatalogView {
    pub fn from_operations(
        operations: impl IntoIterator<Item = SignedCatalogOperation>,
        project_id: &str,
        recovery_epoch: u64,
        membership: &MembershipView,
    ) -> Result<Self, CatalogError> {
        let mut view = Self::default();
        for operation in operations {
            view.apply(operation, project_id, recovery_epoch, membership)?;
        }
        Ok(view)
    }

    pub fn apply(
        &mut self,
        operation: SignedCatalogOperation,
        project_id: &str,
        recovery_epoch: u64,
        membership: &MembershipView,
    ) -> Result<CatalogApply, CatalogError> {
        operation.verify(project_id, recovery_epoch, membership)?;
        if self.operation_ids.contains(&operation.operation_id) {
            return Ok(CatalogApply::Duplicate);
        }
        let replica_owner_epoch = self
            .records
            .values()
            .filter(|current| current.record.replica_id == operation.record.replica_id)
            .map(|current| current.record.owner_epoch)
            .max();
        if replica_owner_epoch.is_some_and(|epoch| operation.record.owner_epoch < epoch) {
            return Err(CatalogError::StaleOwnerEpoch);
        }
        if let Some(current) = self.records.get(&operation.record.deployment_id) {
            if operation.record.owner_epoch < current.record.owner_epoch {
                return Err(CatalogError::StaleOwnerEpoch);
            }
            if order(&operation) <= order(current) {
                self.operation_ids.insert(operation.operation_id);
                return Ok(CatalogApply::Superseded);
            }
            if current.record.state == DeploymentState::Tombstoned
                && operation.record.state != DeploymentState::Tombstoned
                && operation.record.owner_epoch == current.record.owner_epoch
            {
                return Err(CatalogError::TombstoneResurrection);
            }
        }
        self.operation_ids.insert(operation.operation_id.clone());
        self.records
            .insert(operation.record.deployment_id.clone(), operation);
        Ok(CatalogApply::Applied)
    }

    /// Active, healthy replicas -- the only records DNS may ever answer with. Candidate,
    /// draining, stopped, and tombstoned records are excluded.
    pub fn active_healthy(&self) -> impl Iterator<Item = &CatalogRecord> {
        let mut winners = BTreeMap::<&str, &SignedCatalogOperation>::new();
        for operation in self.records.values().filter(|operation| {
            operation.record.state == DeploymentState::Active
                && operation.record.health == HealthState::Healthy
        }) {
            let replace = winners
                .get(operation.record.replica_id.as_str())
                .is_none_or(|current| order(operation) > order(current));
            if replace {
                winners.insert(operation.record.replica_id.as_str(), operation);
            }
        }
        winners.into_values().map(|operation| &operation.record)
    }

    pub fn get(&self, replica_id: &str) -> Option<&CatalogRecord> {
        self.records
            .values()
            .filter(|operation| operation.record.replica_id == replica_id)
            .max_by_key(|operation| order(operation))
            .map(|operation| &operation.record)
    }

    pub fn all(&self) -> impl Iterator<Item = &CatalogRecord> {
        self.records.values().map(|op| &op.record)
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

fn order(operation: &SignedCatalogOperation) -> (u64, u64, u8, &str) {
    (
        operation.record.owner_epoch,
        operation.record.revision,
        match operation.record.state {
            DeploymentState::Tombstoned => 1,
            _ => 0,
        },
        &operation.operation_id,
    )
}

fn operation_id(record: &CatalogRecord) -> Result<String, CatalogError> {
    let digest = Sha256::digest(serde_json::to_vec(record)?);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
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
    #[error("catalog operation ID does not match its body")]
    InvalidOperationId,
    #[error("catalog operation has an invalid signature")]
    InvalidSignature,
    #[error("catalog operation was not signed by an active member node")]
    UnknownSigner,
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
        MembershipRecord, MembershipState, SignedMembership, MEMBERSHIP_PROTOCOL_VERSION,
        MEMBERSHIP_SCHEMA_VERSION,
    };
    use std::net::SocketAddr;

    fn node_key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn membership_with(node_id: &str, key: &SigningKey) -> MembershipView {
        let signer_key = SigningKey::from_bytes(&[99; 32]);
        let mut authority = crate::membership::AuthorityKeyring::new("project", 1);
        authority.add_authority("root", signer_key.verifying_key());
        let record = MembershipRecord {
            project_id: "project".into(),
            recovery_epoch: 1,
            protocol_version: MEMBERSHIP_PROTOCOL_VERSION,
            schema_version: MEMBERSHIP_SCHEMA_VERSION,
            node_id: node_id.into(),
            server_name: node_id.into(),
            node_signing_public_key: key.verifying_key().to_bytes().to_vec(),
            wireguard_public_key: format!("wg-{node_id}"),
            management_address: "100.98.64.1".parse().unwrap(),
            container_subnet: "198.18.1.0/24".into(),
            endpoints: vec!["192.0.2.1:51820".parse::<SocketAddr>().unwrap()],
            owner_epoch: 1,
            revision: 1,
            state: MembershipState::Active,
        };
        let signed = SignedMembership::sign(record, "root", &signer_key).unwrap();
        let mut view = MembershipView::default();
        view.apply(signed, &authority).unwrap();
        view
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
    fn a_node_can_sign_and_verify_its_own_replica() {
        let key = node_key(1);
        let membership = membership_with("node-a", &key);
        let signed =
            SignedCatalogOperation::sign(record("r1", "node-a", 1, DeploymentState::Active), &key)
                .unwrap();
        signed.verify("project", 1, &membership).unwrap();
    }

    #[test]
    fn a_valid_member_cannot_claim_ownership_of_another_nodes_replica() {
        // `SignedCatalogOperation::sign` always derives `signer_id` from `record.owner_node_id`,
        // so this attacks the wire shape directly: a legitimate, active member (node-a) signs the
        // record's operation_id with its own key, but hand-sets `signer_id` to itself while the
        // record claims ownership by node-b. `signer_id` alone is authenticated (it's what gets
        // looked up in membership); without the explicit ownership check this would otherwise
        // verify cleanly.
        let key = node_key(1);
        let membership = membership_with("node-a", &key);
        let record = record("r1", "node-b", 1, DeploymentState::Active);
        let operation_id = operation_id(&record).unwrap();
        let signature = key.sign(operation_id.as_bytes()).to_bytes().to_vec();
        let forged = SignedCatalogOperation {
            operation_id,
            signer_id: "node-a".into(),
            record,
            signature,
        };
        assert!(matches!(
            forged.verify("project", 1, &membership),
            Err(CatalogError::NotOwner)
        ));
    }

    #[test]
    fn unsigned_by_any_known_member_is_rejected() {
        let key = node_key(1);
        let membership = MembershipView::default();
        let signed =
            SignedCatalogOperation::sign(record("r1", "node-a", 1, DeploymentState::Active), &key)
                .unwrap();
        assert!(matches!(
            signed.verify("project", 1, &membership),
            Err(CatalogError::UnknownSigner)
        ));
    }

    #[test]
    fn duplicate_and_out_of_order_delivery_converge() {
        let key = node_key(1);
        let membership = membership_with("node-a", &key);
        let older = SignedCatalogOperation::sign(
            record("r1", "node-a", 1, DeploymentState::Candidate),
            &key,
        )
        .unwrap();
        let newer =
            SignedCatalogOperation::sign(record("r1", "node-a", 2, DeploymentState::Active), &key)
                .unwrap();
        let mut view = CatalogView::default();
        assert_eq!(
            view.apply(newer.clone(), "project", 1, &membership)
                .unwrap(),
            CatalogApply::Applied
        );
        assert_eq!(
            view.apply(older, "project", 1, &membership).unwrap(),
            CatalogApply::Superseded
        );
        assert_eq!(
            view.apply(newer, "project", 1, &membership).unwrap(),
            CatalogApply::Duplicate
        );
        assert_eq!(view.get("r1").unwrap().state, DeploymentState::Active);
    }

    #[test]
    fn candidate_and_draining_records_are_excluded_from_active_healthy() {
        let key = node_key(1);
        let membership = membership_with("node-a", &key);
        let mut view = CatalogView::default();
        for (id, state) in [
            ("r-candidate", DeploymentState::Candidate),
            ("r-active", DeploymentState::Active),
            ("r-draining", DeploymentState::Draining),
            ("r-stopped", DeploymentState::Stopped),
        ] {
            let signed =
                SignedCatalogOperation::sign(record(id, "node-a", 1, state), &key).unwrap();
            view.apply(signed, "project", 1, &membership).unwrap();
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
        let key = node_key(1);
        let membership = membership_with("node-a", &key);
        let mut view = CatalogView::default();
        let mut tombstone = record("r1", "node-a", 2, DeploymentState::Tombstoned);
        tombstone.owner_epoch = 1;
        view.apply(
            SignedCatalogOperation::sign(tombstone, &key).unwrap(),
            "project",
            1,
            &membership,
        )
        .unwrap();
        let replay = record("r1", "node-a", 3, DeploymentState::Active);
        assert!(matches!(
            view.apply(
                SignedCatalogOperation::sign(replay, &key).unwrap(),
                "project",
                1,
                &membership
            ),
            Err(CatalogError::TombstoneResurrection)
        ));
        let mut replacement = record("r1", "node-a", 1, DeploymentState::Active);
        replacement.owner_epoch = 2;
        view.apply(
            SignedCatalogOperation::sign(replacement, &key).unwrap(),
            "project",
            1,
            &membership,
        )
        .unwrap();
        assert_eq!(view.active_healthy().count(), 1);
    }

    #[test]
    fn wrong_project_and_recovery_epoch_are_rejected() {
        let key = node_key(1);
        let membership = membership_with("node-a", &key);
        let signed =
            SignedCatalogOperation::sign(record("r1", "node-a", 1, DeploymentState::Active), &key)
                .unwrap();
        assert!(matches!(
            signed.verify("other-project", 1, &membership),
            Err(CatalogError::WrongProject)
        ));
        assert!(matches!(
            signed.verify("project", 2, &membership),
            Err(CatalogError::RecoveryEpoch { .. })
        ));
    }
}
