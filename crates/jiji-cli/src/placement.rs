use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicaAssignment {
    pub replica_id: String,
    pub ordinal: u32,
    pub server: String,
}

/// A replica's id depends only on its own `(server, local_index)` pair: adding/
/// removing an unrelated server, or changing `scale`, never reassigns any other
/// replica's id.
pub fn replica_id_for(project: &str, service: &str, server: &str, local_index: u32) -> String {
    let digest =
        Sha256::digest(format!("{project}\0{service}\0{server}\0{local_index}").as_bytes());
    format!(
        "{}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        service, digest[0], digest[1], digest[2], digest[3], digest[4], digest[5]
    )
}

/// Every listed server is a literal deploy target: `scale` instances land on each
/// one, not a total spread across a pool. Servers are sorted+dedup'd so YAML map
/// order, SSH connection order, and the node that computes the plan cannot alter it.
/// `ordinal` is a plain secondary sort key (position in the sorted
/// `(server, local_index)` enumeration), not part of replica identity.
pub fn assignments_for(
    project: &str,
    service: &str,
    servers: &[String],
    scale: u32,
) -> Vec<ReplicaAssignment> {
    let mut servers = servers.to_vec();
    servers.sort();
    servers.dedup();

    let mut ordinal = 0;
    let mut assignments = Vec::new();
    for server in &servers {
        for local_index in 0..scale {
            assignments.push(ReplicaAssignment {
                replica_id: replica_id_for(project, service, server, local_index),
                ordinal,
                server: server.clone(),
            });
            ordinal += 1;
        }
    }
    assignments
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_replica_identity_does_not_include_owner() {
        assert_eq!(
            replica_id_for("demo", "web", "a", 0),
            replica_id_for("demo", "web", "a", 0)
        );
        assert_ne!(
            replica_id_for("demo", "web", "a", 0),
            replica_id_for("demo", "web", "a", 1)
        );
    }

    #[test]
    fn replica_identity_is_stable_across_unrelated_server_changes() {
        // Adding/removing an unrelated server never reassigns "a"'s local_index 0 id.
        let two_servers = assignments_for("demo", "web", &["a".into(), "b".into()], 1);
        let three_servers =
            assignments_for("demo", "web", &["a".into(), "b".into(), "c".into()], 1);
        let a_id_two = two_servers
            .iter()
            .find(|a| a.server == "a")
            .unwrap()
            .replica_id
            .clone();
        let a_id_three = three_servers
            .iter()
            .find(|a| a.server == "a")
            .unwrap()
            .replica_id
            .clone();
        assert_eq!(a_id_two, a_id_three);
    }

    #[test]
    fn replica_identity_is_stable_across_scale_changes() {
        let scale_one = assignments_for("demo", "web", &["a".into()], 1);
        let scale_two = assignments_for("demo", "web", &["a".into()], 2);
        assert_eq!(scale_one[0].replica_id, scale_two[0].replica_id);
    }

    #[test]
    fn assignments_for_produces_one_per_server_per_local_index() {
        let servers = vec!["b".into(), "a".into()];
        let assignments = assignments_for("demo", "web", &servers, 2);
        assert_eq!(assignments.len(), 4);
        assert_eq!(
            assignments
                .iter()
                .map(|a| a.server.as_str())
                .collect::<Vec<_>>(),
            ["a", "a", "b", "b"]
        );
        assert_eq!(
            assignments.iter().map(|a| a.ordinal).collect::<Vec<_>>(),
            [0, 1, 2, 3]
        );
    }

    #[test]
    fn assignments_for_dedups_duplicate_servers() {
        let servers = vec!["a".into(), "a".into()];
        let assignments = assignments_for("demo", "web", &servers, 1);
        assert_eq!(assignments.len(), 1);
    }
}
