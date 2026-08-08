//! Durable, host-local deployment address allocation.
//!
//! The allocator deliberately receives infrastructure reservations from the
//! caller. That keeps it independent of a compiled cluster topology while
//! making gateway, DNS, proxy, network, and broadcast addresses impossible to
//! lease accidentally.

use std::collections::BTreeSet;
use std::net::Ipv4Addr;

use jiji_network::Ipv4Cidr;
use thiserror::Error;

use crate::store::{AddressLease, AgentStore, StoreError};

pub const DEFAULT_QUARANTINE_SECONDS: u64 = 30;

#[derive(Debug, Error)]
pub enum LeaseError {
    #[error(
        "deployment '{deployment_id}' already owns {actual}, not requested address {requested}"
    )]
    DeploymentConflict {
        deployment_id: String,
        actual: Ipv4Addr,
        requested: Ipv4Addr,
    },
    #[error("no deployment addresses remain in {subnet}")]
    Exhausted { subnet: Ipv4Cidr },
    #[error("lease store failed: {0}")]
    Store(#[from] StoreError),
}

/// The synthetic `replica_id` a cron run's address lease uses (`AddressAllocator::allocate`'s
/// `deployment_id` is the run's own `run_id` -- see `cron_exec.rs`'s module doc comment for why a
/// cron run needs no `deployment_id` distinct from its `run_id`). Distinguishes a cron lease from
/// a real service replica's in diagnostics without a schema change: the plan's "local cron-run
/// claim type" is this naming convention over the existing generic `address_leases` table, not a
/// new one -- a cron run has no `CatalogRecord`/replica placement of its own to key against.
pub fn cron_replica_id(service: &str, cron_name: &str) -> String {
    format!("cron/{service}/{cron_name}")
}

pub struct AddressAllocator<'a> {
    store: &'a AgentStore,
    subnet: Ipv4Cidr,
    reserved: BTreeSet<Ipv4Addr>,
}

impl<'a> AddressAllocator<'a> {
    pub fn new(
        store: &'a AgentStore,
        subnet: Ipv4Cidr,
        reserved: impl IntoIterator<Item = Ipv4Addr>,
    ) -> Self {
        let mut reserved = reserved.into_iter().collect::<BTreeSet<_>>();
        reserved.insert(subnet.network());
        if let Some(broadcast) = subnet.address(subnet.address_count().saturating_sub(1)) {
            reserved.insert(broadcast);
        }
        Self {
            store,
            subnet,
            reserved,
        }
    }

    pub fn allocate(
        &self,
        deployment_id: &str,
        replica_id: &str,
        timestamp: u64,
    ) -> Result<AddressLease, LeaseError> {
        self.store.collect_expired_address_leases(timestamp)?;
        if let Some(existing) = self.store.address_lease(deployment_id)? {
            return Ok(existing);
        }

        let used = self
            .store
            .address_leases()?
            .into_iter()
            .map(|lease| lease.address)
            .collect::<BTreeSet<_>>();
        for offset in 1..self.subnet.address_count().saturating_sub(1) {
            let Some(address) = self.subnet.address(offset) else {
                continue;
            };
            if self.reserved.contains(&address) || used.contains(&address) {
                continue;
            }
            if self
                .store
                .claim_address_lease(deployment_id, replica_id, address)?
            {
                return Ok(self
                    .store
                    .address_lease(deployment_id)?
                    .expect("a successful claim must be readable"));
            }
        }
        Err(LeaseError::Exhausted {
            subnet: self.subnet,
        })
    }

    pub fn release(
        &self,
        deployment_id: &str,
        timestamp: u64,
        quarantine_seconds: u64,
    ) -> Result<bool, LeaseError> {
        Ok(self.store.quarantine_address_lease(
            deployment_id,
            timestamp.saturating_add(quarantine_seconds),
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store(temp: &TempDir) -> AgentStore {
        AgentStore::open(&temp.path().join("agent.db")).unwrap()
    }

    #[test]
    fn cron_replica_id_is_distinct_per_job_and_never_collides_with_a_real_replica_id() {
        assert_eq!(
            cron_replica_id("twitch", "sync-twitch"),
            "cron/twitch/sync-twitch"
        );
        assert_ne!(
            cron_replica_id("twitch", "sync-twitch"),
            cron_replica_id("twitch", "cleanup")
        );
    }

    #[test]
    fn allocates_idempotently_and_skips_infrastructure() {
        let temp = TempDir::new().unwrap();
        let store = store(&temp);
        let subnet = "10.20.0.0/29".parse().unwrap();
        let allocator = AddressAllocator::new(
            &store,
            subnet,
            ["10.20.0.1", "10.20.0.2", "10.20.0.3"]
                .into_iter()
                .map(|value| value.parse().unwrap()),
        );
        let first = allocator.allocate("deploy-a", "replica-a", 10).unwrap();
        assert_eq!(first.address, "10.20.0.4".parse::<Ipv4Addr>().unwrap());
        assert_eq!(
            allocator.allocate("deploy-a", "replica-a", 11).unwrap(),
            first
        );
        let second = allocator.allocate("deploy-b", "replica-b", 11).unwrap();
        assert_eq!(second.address, "10.20.0.5".parse::<Ipv4Addr>().unwrap());
    }

    #[test]
    fn quarantines_before_reuse_and_recovers_after_restart() {
        let temp = TempDir::new().unwrap();
        let subnet = "10.30.0.0/30".parse().unwrap();
        {
            let store = store(&temp);
            let allocator = AddressAllocator::new(&store, subnet, ["10.30.0.2".parse().unwrap()]);
            let lease = allocator.allocate("deploy-a", "replica-a", 10).unwrap();
            assert_eq!(lease.address, "10.30.0.1".parse::<Ipv4Addr>().unwrap());
            allocator.release("deploy-a", 20, 30).unwrap();
            assert!(matches!(
                allocator.allocate("deploy-b", "replica-b", 49),
                Err(LeaseError::Exhausted { .. })
            ));
        }
        let reopened = store(&temp);
        reopened.set_checkpoint("last_discovery_at", "49").unwrap();
        let allocator = AddressAllocator::new(&reopened, subnet, ["10.30.0.2".parse().unwrap()]);
        let lease = allocator.allocate("deploy-b", "replica-b", 50).unwrap();
        assert_eq!(lease.address, "10.30.0.1".parse::<Ipv4Addr>().unwrap());
    }

    #[test]
    fn timeout_alone_never_collects_a_lease_still_claimed_by_inventory() {
        let temp = TempDir::new().unwrap();
        let store = store(&temp);
        let subnet = "10.40.0.0/30".parse().unwrap();
        let allocator = AddressAllocator::new(&store, subnet, ["10.40.0.2".parse().unwrap()]);
        allocator.allocate("deploy-a", "replica-a", 1).unwrap();
        allocator.release("deploy-a", 2, 3).unwrap();

        assert!(matches!(
            allocator.allocate("deploy-b", "replica-b", 100),
            Err(LeaseError::Exhausted { .. })
        ));
        store
            .upsert_observation(&crate::store::Observation {
                container_id: "container-a".into(),
                name: "demo-a".into(),
                image: "nginx".into(),
                labels_json: serde_json::json!({
                    "jiji.deployment": "deploy-a"
                })
                .to_string(),
                state: "exited".into(),
            })
            .unwrap();
        store.set_checkpoint("last_discovery_at", "100").unwrap();
        assert!(matches!(
            allocator.allocate("deploy-b", "replica-b", 100),
            Err(LeaseError::Exhausted { .. })
        ));

        store.retain_observations(&[]).unwrap();
        let replacement = allocator.allocate("deploy-b", "replica-b", 101).unwrap();
        assert_eq!(
            replacement.address,
            "10.40.0.1".parse::<Ipv4Addr>().unwrap()
        );
    }
}
