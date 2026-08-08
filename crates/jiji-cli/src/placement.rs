use std::collections::BTreeMap;

use jiji_config::{PlacementPolicy, Service};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicaAssignment {
    pub replica_id: String,
    pub ordinal: u32,
    pub server: String,
}

pub fn replica_id(project: &str, service: &str, ordinal: u32) -> String {
    let digest = Sha256::digest(format!("{project}\0{service}\0{ordinal}").as_bytes());
    format!(
        "{}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        service, digest[0], digest[1], digest[2], digest[3], digest[4], digest[5]
    )
}

/// Return the stable identity used by the legacy one-endpoint-per-server
/// command surface while it is being migrated to replica assignments.
pub fn endpoint_replica_id(
    project: &str,
    service_name: &str,
    service: &Service,
    server_name: &str,
) -> anyhow::Result<String> {
    let mut servers = service.servers.clone();
    servers.sort();
    servers.dedup();
    let ordinal = servers
        .iter()
        .position(|server| server == server_name)
        .ok_or_else(|| {
            anyhow::anyhow!("Server '{server_name}' is not eligible for service '{service_name}'")
        })? as u32;
    Ok(replica_id(project, service_name, ordinal))
}

/// Deterministic initial placement. Eligibility is sorted so YAML map order,
/// SSH connection order, and the node that computes the plan cannot alter it.
pub fn place(
    project: &str,
    service: &str,
    replicas: u32,
    eligible_servers: &[String],
    policy: PlacementPolicy,
) -> Vec<ReplicaAssignment> {
    let mut servers = eligible_servers.to_vec();
    servers.sort();
    servers.dedup();
    if servers.is_empty() {
        return Vec::new();
    }

    let mut loads =
        BTreeMap::<String, u32>::from_iter(servers.iter().cloned().map(|server| (server, 0)));
    (0..replicas)
        .map(|ordinal| {
            let server = match policy {
                PlacementPolicy::Spread => loads
                    .iter()
                    .min_by_key(|(server, count)| (**count, server.as_str()))
                    .map(|(server, _)| server.clone())
                    .expect("eligible server set is non-empty"),
                PlacementPolicy::Packed => servers[0].clone(),
            };
            *loads.get_mut(&server).expect("selected server exists") += 1;
            ReplicaAssignment {
                replica_id: replica_id(project, service, ordinal),
                ordinal,
                server,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_replica_identity_does_not_include_owner() {
        assert_eq!(replica_id("demo", "web", 2), replica_id("demo", "web", 2));
        assert_ne!(replica_id("demo", "web", 2), replica_id("demo", "web", 3));
    }

    #[test]
    fn spread_is_balanced_and_input_order_independent() {
        let left = place(
            "demo",
            "web",
            5,
            &["b".into(), "a".into()],
            PlacementPolicy::Spread,
        );
        let right = place(
            "demo",
            "web",
            5,
            &["a".into(), "b".into()],
            PlacementPolicy::Spread,
        );
        assert_eq!(left, right);
        assert_eq!(
            left.iter()
                .map(|item| item.server.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "a", "b", "a"]
        );
    }

    #[test]
    fn packed_uses_deterministic_first_host() {
        let plan = place(
            "demo",
            "worker",
            3,
            &["z".into(), "a".into()],
            PlacementPolicy::Packed,
        );
        assert!(plan.iter().all(|item| item.server == "a"));
    }

    #[test]
    fn endpoint_identity_is_independent_of_config_order() {
        let service: Service =
            serde_yaml::from_str("image: example/web\nservers: [b, a]\n").unwrap();
        assert_eq!(
            endpoint_replica_id("demo", "web", &service, "b").unwrap(),
            replica_id("demo", "web", 1)
        );
    }
}
