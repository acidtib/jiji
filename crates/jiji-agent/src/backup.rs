//! Secret-free, consistent control-plane export from one agent store.
//!
//! Encryption is an operator/CLI responsibility. This payload intentionally excludes the local
//! node signing seed, deployed environment, SSH material, and WireGuard private key. Membership
//! itself is never part of this snapshot: it's trivially re-derived from `jiji.yml` and pushed
//! fresh by `jiji-cli` (the same computation `server setup` already does), not backed up per host
//! -- see `crate::membership`'s module doc comment. `import` therefore assumes the target store's
//! membership is already current before it runs.

use serde::{Deserialize, Serialize};

use crate::catalog::CatalogRecord;
use crate::desired::DesiredStateRecord;
use crate::membership::{MembershipScope, MembershipView, RecordProvenance};
use crate::store::{AddressLease, AgentStore, StoreError};

pub const BACKUP_FORMAT_VERSION: u16 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentBackupSnapshot {
    pub format_version: u16,
    pub project_id: String,
    pub recovery_epoch: u64,
    pub node_id: String,
    pub catalog: Vec<CatalogRecord>,
    pub desired: Vec<DesiredStateRecord>,
    pub address_leases: Vec<AddressLease>,
}

impl AgentBackupSnapshot {
    pub fn export(
        store: &AgentStore,
        project_id: &str,
        recovery_epoch: u64,
        node_id: &str,
    ) -> Result<Self, StoreError> {
        Ok(Self {
            format_version: BACKUP_FORMAT_VERSION,
            project_id: project_id.to_string(),
            recovery_epoch,
            node_id: node_id.to_string(),
            catalog: store.latest_catalog()?,
            desired: store.desired_snapshot_operations()?,
            address_leases: store.address_leases()?,
        })
    }

    pub fn validate_identity(&self, project_id: &str, recovery_epoch: u64) -> anyhow::Result<()> {
        if self.format_version != BACKUP_FORMAT_VERSION {
            anyhow::bail!("unsupported agent backup format {}", self.format_version);
        }
        if self.project_id != project_id {
            anyhow::bail!("agent backup belongs to another project");
        }
        if self.recovery_epoch != recovery_epoch {
            anyhow::bail!("agent backup belongs to another recovery epoch");
        }
        Ok(())
    }

    /// Merges a same-epoch snapshot into a surviving/rebuilt local store. Each record already
    /// passed verification once, on the host it was exported from (`RecordProvenance::Verified`),
    /// so restoring it here is a replay, not a new local write. Local leases are recovered
    /// conflict-refusing. Assumes the target's membership is already current -- the caller must
    /// push fresh membership first (see the module doc comment) -- since a record's
    /// `author_epoch`/ownership is still checked against it.
    pub fn import(
        &self,
        store: &AgentStore,
        project_id: &str,
        recovery_epoch: u64,
    ) -> anyhow::Result<()> {
        self.validate_identity(project_id, recovery_epoch)?;
        let scope = MembershipScope::new(project_id, recovery_epoch);
        let membership = MembershipView::from_records(store.membership_operations()?, &scope)?;
        for record in &self.catalog {
            store.apply_catalog(
                record.clone(),
                RecordProvenance::Verified,
                project_id,
                recovery_epoch,
                &membership,
            )?;
        }
        for record in &self.desired {
            store.apply_desired(
                record.clone(),
                RecordProvenance::Verified,
                project_id,
                recovery_epoch,
                &membership,
            )?;
        }
        for lease in &self.address_leases {
            if !store.recover_address_lease(
                &lease.deployment_id,
                &lease.replica_id,
                lease.address,
            )? {
                anyhow::bail!(
                    "address claim {} for deployment '{}' conflicts with surviving local state",
                    lease.address,
                    lease.deployment_id
                );
            }
            if lease.state == "quarantined" {
                store.quarantine_address_lease(
                    &lease.deployment_id,
                    lease.quarantine_until.unwrap_or(u64::MAX),
                )?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn export_contains_local_claims_but_no_private_key_field() {
        let dir = tempdir().unwrap();
        let store = AgentStore::open(&dir.path().join("agent.sqlite3")).unwrap();
        store
            .claim_address_lease("deploy-1", "replica-1", "10.0.0.4".parse().unwrap())
            .unwrap();
        let snapshot = AgentBackupSnapshot::export(&store, "demo", 2, "node-a").unwrap();
        snapshot.validate_identity("demo", 2).unwrap();
        assert_eq!(snapshot.address_leases.len(), 1);
        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(!json.contains("signing_key"));
        assert!(!json.contains("private_key"));
        assert!(snapshot.validate_identity("other", 2).is_err());

        let restored_dir = tempdir().unwrap();
        let restored = AgentStore::open(&restored_dir.path().join("agent.sqlite3")).unwrap();
        snapshot.import(&restored, "demo", 2).unwrap();
        assert_eq!(restored.address_leases().unwrap(), snapshot.address_leases);
    }
}
