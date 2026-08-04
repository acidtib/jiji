//! Incremental WireGuard reconciliation derived only from authenticated
//! membership records. Service/catalog changes never enter this module.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

use crate::membership::{MembershipRecord, MembershipState};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerCacheEntry {
    pub node_id: String,
    pub wireguard_public_key: String,
    pub management_address: String,
    pub container_subnet: String,
    pub endpoint: SocketAddr,
    pub owner_epoch: u64,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerAction {
    Set {
        node_id: String,
        public_key: String,
        endpoint: SocketAddr,
        allowed_ips: Vec<String>,
    },
    /// Updates only the peer's endpoint, deliberately never touching `allowed-ips`. `wg set`
    /// atomically clears and re-adds a peer's allowed-ips whenever the `allowed-ips` argument is
    /// given at all, even to an unchanged value -- confirmed live, this produced a brief window
    /// where the kernel had no route to that peer's container subnet, surfacing as "no route to
    /// host" for any cross-host traffic in flight at that exact moment (e.g. a health check
    /// against another host's replica). A roaming NAT endpoint (this module's whole reason for
    /// preferring the currently *observed* endpoint over the configured one) changes far more
    /// often than a peer's actual subnet does, so this case has to stay cheap and side-effect-free.
    UpdateEndpoint {
        node_id: String,
        public_key: String,
        endpoint: SocketAddr,
    },
    Remove {
        node_id: String,
        public_key: String,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcilePlan {
    pub actions: Vec<PeerAction>,
    pub next_cache: BTreeMap<String, PeerCacheEntry>,
}

/// Plans idempotent `wg set` operations.
///
/// `records` may be a partial replicated view. Absence is therefore never
/// deletion authority: a cached peer is removed only when an authenticated
/// tombstone for that exact node is present. A currently observed WireGuard
/// endpoint wins over configured candidates so NAT roaming is preserved.
pub fn plan_reconciliation(
    local_node_id: &str,
    records: &[MembershipRecord],
    cache: &BTreeMap<String, PeerCacheEntry>,
    observed_endpoints: &BTreeMap<String, SocketAddr>,
) -> ReconcilePlan {
    let mut plan = ReconcilePlan {
        actions: Vec::new(),
        next_cache: cache.clone(),
    };
    let mut handled = BTreeSet::new();
    for record in records {
        if record.node_id == local_node_id || !handled.insert(record.node_id.clone()) {
            continue;
        }
        match record.state {
            MembershipState::Tombstoned => {
                if let Some(previous) = plan.next_cache.remove(&record.node_id) {
                    plan.actions.push(PeerAction::Remove {
                        node_id: record.node_id.clone(),
                        public_key: previous.wireguard_public_key,
                    });
                }
            }
            MembershipState::Active => {
                let previous = plan.next_cache.get(&record.node_id);
                if let Some(previous) = previous {
                    if previous.wireguard_public_key != record.wireguard_public_key {
                        plan.actions.push(PeerAction::Remove {
                            node_id: record.node_id.clone(),
                            public_key: previous.wireguard_public_key.clone(),
                        });
                    }
                }
                let endpoint = observed_endpoints
                    .get(&record.wireguard_public_key)
                    .copied()
                    .or_else(|| {
                        previous
                            .filter(|entry| {
                                entry.wireguard_public_key == record.wireguard_public_key
                            })
                            .map(|entry| entry.endpoint)
                    })
                    .or_else(|| record.endpoints.first().copied())
                    .expect("validated active membership has an endpoint");
                let allowed_ips = vec![
                    format!("{}/32", record.management_address),
                    record.container_subnet.clone(),
                ];
                let next = PeerCacheEntry {
                    node_id: record.node_id.clone(),
                    wireguard_public_key: record.wireguard_public_key.clone(),
                    management_address: record.management_address.to_string(),
                    container_subnet: record.container_subnet.clone(),
                    endpoint,
                    owner_epoch: record.owner_epoch,
                    revision: record.revision,
                };
                // Whether this peer's actual allowed-ips (and thus its kernel routes) are
                // unchanged from the cache -- as opposed to just its endpoint, revision, or
                // owner_epoch, none of which affect what `wg set ... allowed-ips` would apply.
                let allowed_ips_unchanged = previous.is_some_and(|previous| {
                    previous.wireguard_public_key == next.wireguard_public_key
                        && previous.management_address == next.management_address
                        && previous.container_subnet == next.container_subnet
                });
                if previous != Some(&next) {
                    if allowed_ips_unchanged {
                        plan.actions.push(PeerAction::UpdateEndpoint {
                            node_id: record.node_id.clone(),
                            public_key: record.wireguard_public_key.clone(),
                            endpoint,
                        });
                    } else {
                        plan.actions.push(PeerAction::Set {
                            node_id: record.node_id.clone(),
                            public_key: record.wireguard_public_key.clone(),
                            endpoint,
                            allowed_ips,
                        });
                    }
                }
                plan.next_cache.insert(record.node_id.clone(), next);
            }
        }
    }
    plan
}

pub fn render_commands(interface: &str, plan: &ReconcilePlan) -> Vec<String> {
    plan.actions
        .iter()
        .map(|action| match action {
            PeerAction::Set {
                public_key,
                endpoint,
                allowed_ips,
                ..
            } => format!(
                "wg set {interface} peer {public_key} endpoint {endpoint} allowed-ips {} persistent-keepalive 25",
                allowed_ips.join(",")
            ),
            PeerAction::UpdateEndpoint {
                public_key,
                endpoint,
                ..
            } => format!("wg set {interface} peer {public_key} endpoint {endpoint}"),
            PeerAction::Remove { public_key, .. } => {
                format!("wg set {interface} peer {public_key} remove")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::membership::{
        MembershipRecord, MEMBERSHIP_PROTOCOL_VERSION, MEMBERSHIP_SCHEMA_VERSION,
    };
    use std::net::Ipv4Addr;

    fn record(node: &str, state: MembershipState) -> MembershipRecord {
        MembershipRecord {
            project_id: "project".into(),
            recovery_epoch: 1,
            protocol_version: MEMBERSHIP_PROTOCOL_VERSION,
            schema_version: MEMBERSHIP_SCHEMA_VERSION,
            node_id: node.into(),
            server_name: node.into(),
            node_signing_public_key: vec![1; 32],
            wireguard_public_key: format!("key-{node}"),
            management_address: Ipv4Addr::new(100, 98, 64, 2),
            container_subnet: "198.18.2.0/24".into(),
            endpoints: vec!["192.0.2.2:51820".parse().unwrap()],
            owner_epoch: 1,
            revision: 1,
            state,
        }
    }

    #[test]
    fn active_peer_is_added_incrementally() {
        let plan = plan_reconciliation(
            "local",
            &[record("remote", MembershipState::Active)],
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        let commands = render_commands("jiji0", &plan);
        assert_eq!(commands.len(), 1);
        assert!(commands[0].contains("wg set jiji0 peer key-remote"));
        assert!(commands[0].contains("allowed-ips 100.98.64.2/32,198.18.2.0/24"));
    }

    #[test]
    fn partial_view_never_removes_a_cached_peer() {
        let cache = BTreeMap::from([(
            "remote".into(),
            PeerCacheEntry {
                node_id: "remote".into(),
                wireguard_public_key: "key-remote".into(),
                management_address: "100.98.64.2".into(),
                container_subnet: "198.18.2.0/24".into(),
                endpoint: "192.0.2.2:51820".parse().unwrap(),
                owner_epoch: 1,
                revision: 1,
            },
        )]);
        let plan = plan_reconciliation("local", &[], &cache, &BTreeMap::new());
        assert!(plan.actions.is_empty());
        assert_eq!(plan.next_cache, cache);
    }

    #[test]
    fn authenticated_tombstone_is_the_only_removal_signal() {
        let active = record("remote", MembershipState::Active);
        let first = plan_reconciliation("local", &[active], &BTreeMap::new(), &BTreeMap::new());
        let tombstone = record("remote", MembershipState::Tombstoned);
        let second =
            plan_reconciliation("local", &[tombstone], &first.next_cache, &BTreeMap::new());
        assert!(matches!(
            second.actions.as_slice(),
            [PeerAction::Remove { .. }]
        ));
        assert!(second.next_cache.is_empty());
    }

    #[test]
    fn live_roaming_endpoint_survives_reconciliation_and_restart_cache() {
        let active = record("remote", MembershipState::Active);
        let roaming = "198.51.100.9:62000".parse().unwrap();
        let observed = BTreeMap::from([("key-remote".into(), roaming)]);
        let first = plan_reconciliation(
            "local",
            std::slice::from_ref(&active),
            &BTreeMap::new(),
            &observed,
        );
        assert_eq!(first.next_cache["remote"].endpoint, roaming);

        let after_restart =
            plan_reconciliation("local", &[active], &first.next_cache, &BTreeMap::new());
        assert!(after_restart.actions.is_empty());
        assert_eq!(after_restart.next_cache["remote"].endpoint, roaming);
    }

    #[test]
    fn roaming_to_a_newly_observed_endpoint_only_updates_the_endpoint() {
        // Regression guard for a live-confirmed bug: re-specifying `allowed-ips` to `wg set` --
        // even with an unchanged value -- atomically clears and re-adds that peer's kernel routes,
        // producing a brief window where cross-host traffic to that peer's subnet sees "no route
        // to host". A roaming NAT endpoint change (this test's exact scenario) must never touch
        // allowed-ips at all.
        let active = record("remote", MembershipState::Active);
        let first = plan_reconciliation(
            "local",
            std::slice::from_ref(&active),
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        assert!(matches!(first.actions.as_slice(), [PeerAction::Set { .. }]));

        let roamed = "198.51.100.9:62000".parse().unwrap();
        let observed = BTreeMap::from([("key-remote".into(), roamed)]);
        let second = plan_reconciliation("local", &[active], &first.next_cache, &observed);
        assert!(matches!(
            second.actions.as_slice(),
            [PeerAction::UpdateEndpoint { endpoint, .. }] if *endpoint == roamed
        ));
        assert_eq!(second.next_cache["remote"].endpoint, roamed);

        let commands = render_commands("jiji0", &second);
        assert_eq!(
            commands,
            vec![format!("wg set jiji0 peer key-remote endpoint {roamed}")]
        );
        assert!(!commands[0].contains("allowed-ips"));
    }

    #[test]
    fn transport_key_rotation_removes_old_key_before_setting_new_key() {
        let old = record("remote", MembershipState::Active);
        let first = plan_reconciliation("local", &[old], &BTreeMap::new(), &BTreeMap::new());
        let mut rotated = record("remote", MembershipState::Active);
        rotated.wireguard_public_key = "key-rotated".into();
        rotated.revision = 2;
        let second = plan_reconciliation("local", &[rotated], &first.next_cache, &BTreeMap::new());
        assert!(matches!(
            second.actions.as_slice(),
            [PeerAction::Remove { .. }, PeerAction::Set { .. }]
        ));
    }

    #[test]
    fn a_genuine_container_subnet_change_still_produces_a_full_set() {
        let old = record("remote", MembershipState::Active);
        let first = plan_reconciliation("local", &[old], &BTreeMap::new(), &BTreeMap::new());
        let mut resized = record("remote", MembershipState::Active);
        resized.container_subnet = "198.18.3.0/24".into();
        resized.revision = 2;
        let second = plan_reconciliation("local", &[resized], &first.next_cache, &BTreeMap::new());
        assert!(matches!(
            second.actions.as_slice(),
            [PeerAction::Set { .. }]
        ));
    }
}
