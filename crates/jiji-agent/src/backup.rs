//! Secret-free, consistent control-plane export from one agent store.
//!
//! Encryption is an operator/CLI responsibility. This payload intentionally excludes the local
//! node signing seed, deployed environment, SSH material, and WireGuard private key.

use serde::{Deserialize, Serialize};

use crate::catalog::SignedCatalogOperation;
use crate::desired::SignedDesiredState;
use crate::membership::SignedMembership;
use crate::membership::{AuthorityKeyring, MembershipView};
use crate::store::{AddressLease, AgentStore, StoreError};

pub const BACKUP_FORMAT_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentBackupSnapshot {
    pub format_version: u16,
    pub project_id: String,
    pub recovery_epoch: u64,
    pub node_id: String,
    pub membership: Vec<SignedMembership>,
    pub catalog: Vec<SignedCatalogOperation>,
    pub desired: Vec<SignedDesiredState>,
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
            membership: store.membership_snapshot_operations()?,
            catalog: store.catalog_snapshot_operations()?,
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

    /// Merges an authenticated same-epoch snapshot into a surviving/rebuilt local store. All
    /// signed records are re-verified; local leases are recovered conflict-refusing. The caller
    /// must use the separate recovery-epoch workflow after total cluster loss.
    pub fn import(&self, store: &AgentStore, authority: &AuthorityKeyring) -> anyhow::Result<()> {
        self.validate_identity(authority.project_id(), authority.recovery_epoch())?;
        for operation in &self.membership {
            store.apply_membership(operation, authority)?;
        }
        let membership =
            MembershipView::from_operations(store.membership_operations()?, authority)?;
        for operation in &self.catalog {
            store.apply_catalog(
                operation,
                authority.project_id(),
                authority.recovery_epoch(),
                &membership,
            )?;
        }
        for operation in &self.desired {
            store.apply_desired(
                operation,
                authority.project_id(),
                authority.recovery_epoch(),
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
        let authority_key = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let mut authority = AuthorityKeyring::new("demo", 2);
        authority.add_authority("root", authority_key.verifying_key());
        snapshot.import(&restored, &authority).unwrap();
        assert_eq!(restored.address_leases().unwrap(), snapshot.address_leases);
    }
}
