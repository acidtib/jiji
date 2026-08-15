//! Replicated desired service placement. See `catalog.rs`'s module doc
//! comment for the `RecordProvenance` authentication model this mirrors.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::membership::{MembershipView, ProvenanceError, RecordProvenance};

pub const DESIRED_PROTOCOL_VERSION: u16 = 1;
pub const DESIRED_SCHEMA_VERSION: u16 = 2;
/// Oldest schema version this build still accepts when replaying durable local history or
/// syncing from a peer. Schema 1's `replica_override` field is read via `#[serde(alias =
/// "replica_override")]` below and its `assignments: Vec<ReplicaAssignment>` field is simply
/// absent from schema 2 and ignored on deserialize, so every schema-1 record on disk remains
/// fully readable. Without this floor, `apply_desired`/`from_records` replaying a node's own
/// pre-upgrade history (or a peer's) would hard-fail on the first schema-1 record it finds,
/// permanently wedging every future desired-state write on that node.
pub const DESIRED_MIN_SUPPORTED_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesiredStateRecord {
    pub project_id: String,
    pub recovery_epoch: u64,
    pub protocol_version: u16,
    pub schema_version: u16,
    pub service: String,
    /// `None` means reset to the configured scale. Per-server instance count override
    /// (every server in `servers:` still gets this many); the actual assignment set is
    /// always recomputed from this plus the service's configured `servers`, never
    /// stored here, so it can never drift out of sync with this value.
    #[serde(alias = "replica_override")]
    pub scale_override: Option<u32>,
    pub revision: u64,
    pub author_node_id: String,
    pub author_epoch: u64,
}

impl DesiredStateRecord {
    fn validate(&self) -> Result<(), DesiredError> {
        if self.protocol_version != DESIRED_PROTOCOL_VERSION {
            return Err(DesiredError::ProtocolVersion(self.protocol_version));
        }
        if self.schema_version < DESIRED_MIN_SUPPORTED_SCHEMA_VERSION
            || self.schema_version > DESIRED_SCHEMA_VERSION
        {
            return Err(DesiredError::SchemaVersion(self.schema_version));
        }
        if self.project_id.is_empty()
            || self.service.is_empty()
            || self.author_node_id.is_empty()
            || self.revision == 0
            || self.author_epoch == 0
        {
            return Err(DesiredError::InvalidRecord);
        }
        Ok(())
    }
}

impl From<ProvenanceError> for DesiredError {
    fn from(error: ProvenanceError) -> Self {
        match error {
            ProvenanceError::UnknownPeer => DesiredError::UnknownAuthor,
            ProvenanceError::NotOwner => DesiredError::NotAuthor,
        }
    }
}

fn verify(
    record: &DesiredStateRecord,
    provenance: RecordProvenance,
    project_id: &str,
    recovery_epoch: u64,
    membership: &MembershipView,
) -> Result<(), DesiredError> {
    record.validate()?;
    if record.project_id != project_id {
        return Err(DesiredError::WrongProject);
    }
    if record.recovery_epoch != recovery_epoch {
        return Err(DesiredError::RecoveryEpoch);
    }
    membership.authenticate(provenance, &record.author_node_id)?;
    let author = membership
        .get(&record.author_node_id)
        .ok_or(DesiredError::UnknownAuthor)?;
    if author.owner_epoch != record.author_epoch {
        return Err(DesiredError::StaleAuthor);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesiredApply {
    Applied,
    Duplicate,
    Superseded,
}

#[derive(Default)]
pub struct DesiredStateView {
    records: BTreeMap<String, DesiredStateRecord>,
    record_ids: BTreeSet<String>,
}

impl DesiredStateView {
    pub fn from_records(
        records: impl IntoIterator<Item = (DesiredStateRecord, RecordProvenance)>,
        project_id: &str,
        recovery_epoch: u64,
        membership: &MembershipView,
    ) -> Result<Self, DesiredError> {
        let mut view = Self::default();
        for (record, provenance) in records {
            view.apply(record, provenance, project_id, recovery_epoch, membership)?;
        }
        Ok(view)
    }

    pub fn apply(
        &mut self,
        record: DesiredStateRecord,
        provenance: RecordProvenance,
        project_id: &str,
        recovery_epoch: u64,
        membership: &MembershipView,
    ) -> Result<DesiredApply, DesiredError> {
        verify(&record, provenance, project_id, recovery_epoch, membership)?;
        let id = record_id(&record)?;
        if !self.record_ids.insert(id.clone()) {
            return Ok(DesiredApply::Duplicate);
        }
        if let Some(current) = self.records.get(&record.service) {
            let current_id = record_id(current)?;
            if order(&record, &id) <= order(current, &current_id) {
                return Ok(DesiredApply::Superseded);
            }
        }
        self.records.insert(record.service.clone(), record);
        Ok(DesiredApply::Applied)
    }

    pub fn get(&self, service: &str) -> Option<&DesiredStateRecord> {
        self.records.get(service)
    }
}

fn order(record: &DesiredStateRecord, id: &str) -> (u64, String) {
    (record.revision, id.to_string())
}

pub(crate) fn record_id(record: &DesiredStateRecord) -> Result<String, DesiredError> {
    Ok(crate::membership::content_hash(record)?)
}

#[derive(Debug, Error)]
pub enum DesiredError {
    #[error("desired state serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("desired state record is invalid")]
    InvalidRecord,
    #[error(
        "desired state protocol version {0} is unsupported (this build understands {DESIRED_PROTOCOL_VERSION}); run `jiji server upgrade` to bring this host's jiji-agent in line with this jiji CLI"
    )]
    ProtocolVersion(u16),
    #[error(
        "desired state schema version {0} is unsupported (this build understands {DESIRED_MIN_SUPPORTED_SCHEMA_VERSION}..={DESIRED_SCHEMA_VERSION}); run `jiji server upgrade` to bring this host's jiji-agent in line with this jiji CLI"
    )]
    SchemaVersion(u16),
    #[error("desired state belongs to another project")]
    WrongProject,
    #[error("desired state belongs to another recovery epoch")]
    RecoveryEpoch,
    #[error("desired state was not sent by a known member's own management address")]
    UnknownAuthor,
    #[error("desired state claims authorship by a node other than the one that sent it")]
    NotAuthor,
    #[error("desired state author epoch is stale")]
    StaleAuthor,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::membership::{
        MembershipRecord, MembershipScope, MembershipState, MEMBERSHIP_PROTOCOL_VERSION,
        MEMBERSHIP_SCHEMA_VERSION,
    };

    fn fixture() -> MembershipView {
        let scope = MembershipScope::new("demo", 1);
        let mut view = MembershipView::default();
        view.apply(
            MembershipRecord {
                project_id: "demo".into(),
                recovery_epoch: 1,
                protocol_version: MEMBERSHIP_PROTOCOL_VERSION,
                schema_version: MEMBERSHIP_SCHEMA_VERSION,
                node_id: "a".into(),
                server_name: "a".into(),
                wireguard_public_key: "wg-a".into(),
                management_address: "100.98.0.1".parse().unwrap(),
                container_subnet: "198.18.0.0/24".into(),
                endpoints: vec!["192.0.2.1:51820".parse().unwrap()],
                owner_epoch: 1,
                revision: 1,
                state: MembershipState::Active,
            },
            &scope,
        )
        .unwrap();
        view
    }

    fn record(revision: u64, scale: u32) -> DesiredStateRecord {
        DesiredStateRecord {
            project_id: "demo".into(),
            recovery_epoch: 1,
            protocol_version: DESIRED_PROTOCOL_VERSION,
            schema_version: DESIRED_SCHEMA_VERSION,
            service: "web".into(),
            scale_override: Some(scale),
            revision,
            author_node_id: "a".into(),
            author_epoch: 1,
        }
    }

    #[test]
    fn newer_desired_state_wins_and_duplicates_are_idempotent() {
        let membership = fixture();
        let first = record(1, 1);
        let second = record(2, 2);
        let mut view = DesiredStateView::default();
        assert_eq!(
            view.apply(
                first.clone(),
                RecordProvenance::Local,
                "demo",
                1,
                &membership
            )
            .unwrap(),
            DesiredApply::Applied
        );
        assert_eq!(
            view.apply(first, RecordProvenance::Local, "demo", 1, &membership)
                .unwrap(),
            DesiredApply::Duplicate
        );
        assert_eq!(
            view.apply(second, RecordProvenance::Local, "demo", 1, &membership)
                .unwrap(),
            DesiredApply::Applied
        );
        assert_eq!(view.get("web").unwrap().scale_override, Some(2));
    }

    #[test]
    fn a_stale_author_epoch_is_rejected() {
        let membership = fixture();
        let mut stale = record(1, 1);
        stale.author_epoch = 2;
        assert!(matches!(
            verify(&stale, RecordProvenance::Local, "demo", 1, &membership),
            Err(DesiredError::StaleAuthor)
        ));
    }

    #[test]
    fn a_still_supported_old_schema_version_is_accepted() {
        // Schema 1's `replica_override` field (aliased above) and dropped `assignments` field
        // must not wedge replay of a node's own pre-upgrade history; see
        // `DESIRED_MIN_SUPPORTED_SCHEMA_VERSION`'s doc comment.
        let membership = fixture();
        let mut old = record(1, 1);
        old.schema_version = 1;
        assert!(verify(&old, RecordProvenance::Local, "demo", 1, &membership).is_ok());
    }

    #[test]
    fn a_genuinely_unsupported_schema_version_is_rejected() {
        let membership = fixture();
        let mut unsupported = record(1, 1);
        unsupported.schema_version = DESIRED_SCHEMA_VERSION + 1;
        assert!(matches!(
            verify(&unsupported, RecordProvenance::Local, "demo", 1, &membership),
            Err(DesiredError::SchemaVersion(v)) if v == DESIRED_SCHEMA_VERSION + 1
        ));
    }

    #[test]
    fn an_unknown_author_is_rejected() {
        let membership = fixture();
        let mut unknown = record(1, 1);
        unknown.author_node_id = "ghost".into();
        assert!(matches!(
            verify(&unknown, RecordProvenance::Local, "demo", 1, &membership),
            Err(DesiredError::UnknownAuthor)
        ));
    }
}
