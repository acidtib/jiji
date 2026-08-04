use std::net::SocketAddr;
use std::path::Path;

use jiji_agent::membership::{MembershipRecord, MembershipState, SignedMembership};
use jiji_agent::AgentPaths;
use jiji_config::{validate_config, Config};
use jiji_ssh::SshSession;
use jiji_tui::Ui;

use crate::{membership_authority::ProjectAuthority, ssh_adapter};

pub enum Change {
    Decommission,
    Endpoint(String),
    RotateKey {
        public_key: String,
        endpoint: String,
    },
    Replace {
        public_key: String,
        endpoint: String,
    },
}

pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    server: &str,
    seed: &str,
    change: Change,
) -> anyhow::Result<()> {
    let start = std::env::current_dir()?;
    let (config, path) =
        crate::config_loading::load_config_for_ssh(environment, config_file.map(Path::new), &start)
            .await?;
    let validation = validate_config(&config);
    if !validation.valid {
        anyhow::bail!("Configuration is invalid; fix it before changing membership");
    }
    if !config.servers.contains_key(server) {
        anyhow::bail!("Unknown membership server '{server}'");
    }
    let seed_config = config
        .servers
        .get(seed)
        .ok_or_else(|| anyhow::anyhow!("Unknown seed server '{seed}'"))?;
    let ssh = config
        .ssh
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No `ssh:` section is configured"))?;
    let options = ssh_adapter::connect_options(seed, seed_config, ssh)?;

    Ui::section("Publishing Mesh Membership:");
    Ui::say(
        &format!("Connecting only to seed {seed} ({})", seed_config.host),
        1,
    );
    let session = SshSession::connect(&options).await?;
    let result = publish(&session, &config, &path, server, change).await;
    session.close().await;
    result?;
    Ui::success("Signed membership published; connected peers will converge asynchronously.");
    Ok(())
}

pub async fn rotate_authority(
    environment: Option<&str>,
    config_file: Option<&str>,
    finalize: bool,
) -> anyhow::Result<()> {
    let start = std::env::current_dir()?;
    let (config, path) =
        crate::config_loading::load_config_for_ssh(environment, config_file.map(Path::new), &start)
            .await?;
    let ssh = config
        .ssh
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No `ssh:` section is configured"))?;

    if finalize {
        if ProjectAuthority::previous(&path)?.is_none() {
            anyhow::bail!("no previous membership authority is awaiting retirement");
        }
        crate::membership_authority::finalize_rotation(&path)?;
        Ui::success(
            "Previous private signing key retired; its public verifier remains for historical operations.",
        );
        return Ok(());
    }

    let rotation = ProjectAuthority::stage_rotation(&path)?;
    let authorities = vec![
        jiji_agent::runtime::AuthorityConfig {
            id: rotation.current.id.clone(),
            public_key: rotation
                .current
                .signing_key
                .verifying_key()
                .to_bytes()
                .to_vec(),
        },
        jiji_agent::runtime::AuthorityConfig {
            id: rotation.next.id.clone(),
            public_key: rotation
                .next
                .signing_key
                .verifying_key()
                .to_bytes()
                .to_vec(),
        },
    ];

    Ui::section("Rotating Membership Authority:");
    let mut names = config.servers.keys().cloned().collect::<Vec<_>>();
    names.sort();
    for name in names {
        let options = ssh_adapter::connect_options(&name, &config.servers[&name], ssh)?;
        let session = SshSession::connect(&options).await.map_err(|error| {
            anyhow::anyhow!(
                "Host '{name}' is unavailable; authority rotation was not committed: {error}"
            )
        })?;
        update_host_authorities(&session, &config.project, authorities.clone()).await?;
        session.close().await;
        Ui::say(&format!("{name}: verifier set updated"), 1);
    }
    rotation.commit()?;
    Ui::success("New authority activated; run again with --finalize after convergence.");
    Ok(())
}

async fn update_host_authorities(
    session: &SshSession,
    project: &str,
    authorities: Vec<jiji_agent::runtime::AuthorityConfig>,
) -> anyhow::Result<()> {
    let paths = AgentPaths::default_for_project(project);
    let result = session
        .execute(&format!("cat {}", paths.mesh_config_path.display()))
        .await?;
    if !result.success {
        anyhow::bail!("could not read mesh configuration from {}", session.host());
    }
    let mut config: jiji_agent::runtime::MeshConfig = serde_json::from_str(&result.stdout)?;
    config.authorities = authorities;
    let write = session
        .execute_with_input(
            &format!(
                "install -m 0600 /dev/stdin {}",
                paths.mesh_config_path.display()
            ),
            &serde_json::to_vec_pretty(&config)?,
        )
        .await?;
    if !write.success {
        anyhow::bail!("could not update verifier set on {}", session.host());
    }
    let restart = session
        .execute(&format!("systemctl restart {}", paths.unit_name))
        .await?;
    if !restart.success {
        anyhow::bail!("agent restart failed on {}", session.host());
    }
    Ok(())
}

async fn publish(
    session: &SshSession,
    config: &Config,
    config_path: &Path,
    server: &str,
    change: Change,
) -> anyhow::Result<()> {
    let paths = AgentPaths::default_for_project(&config.project);
    let export = session
        .execute(&format!(
            "{} membership-export --state-dir {}",
            paths.binary_path.display(),
            paths.state_dir.display()
        ))
        .await?;
    if !export.success {
        anyhow::bail!(
            "Could not read signed membership from seed {}: {}",
            session.host(),
            export.stderr.trim()
        );
    }
    let operations: Vec<SignedMembership> = serde_json::from_str(&export.stdout)?;
    let current = latest_record(&operations, server)
        .ok_or_else(|| anyhow::anyhow!("Seed has no membership record for '{server}'"))?;
    let authority = ProjectAuthority::load_or_create(config_path)?;
    let records = changed_records(current, change)?;
    let signed = records
        .into_iter()
        .map(|record| SignedMembership::sign(record, &authority.id, &authority.signing_key))
        .collect::<Result<Vec<_>, _>>()?;
    let update_path = paths.project_dir.join("membership-update.json");
    let write = session
        .execute_with_input(
            &format!("install -m 0600 /dev/stdin {}", update_path.display()),
            &serde_json::to_vec(&signed)?,
        )
        .await?;
    if !write.success {
        anyhow::bail!("Could not stage membership update on seed");
    }
    let apply = session
        .execute(&format!(
            "systemctl stop {unit}; \
             if {binary} membership-import --project {project} --state-dir {state} \
             --mesh-config {mesh} --input {update}; then systemctl start {unit}; \
             else systemctl start {unit}; exit 1; fi",
            unit = paths.unit_name,
            binary = paths.binary_path.display(),
            project = config.project,
            state = paths.state_dir.display(),
            mesh = paths.mesh_config_path.display(),
            update = update_path.display(),
        ))
        .await?;
    if !apply.success {
        anyhow::bail!(
            "Seed rejected the signed membership update: {}",
            apply.stderr.trim()
        );
    }
    Ok(())
}

fn latest_record(operations: &[SignedMembership], node_id: &str) -> Option<MembershipRecord> {
    operations
        .iter()
        .filter(|operation| operation.record.node_id == node_id)
        .max_by_key(|operation| {
            (
                operation.record.owner_epoch,
                operation.record.revision,
                operation.record.state == MembershipState::Tombstoned,
            )
        })
        .map(|operation| operation.record.clone())
}

fn changed_records(
    mut current: MembershipRecord,
    change: Change,
) -> anyhow::Result<Vec<MembershipRecord>> {
    match change {
        Change::Decommission => {
            current.revision += 1;
            current.state = MembershipState::Tombstoned;
            Ok(vec![current])
        }
        Change::Endpoint(endpoint) => {
            current.revision += 1;
            current.endpoints = vec![parse_endpoint(&endpoint)?];
            current.state = MembershipState::Active;
            Ok(vec![current])
        }
        Change::RotateKey {
            public_key,
            endpoint,
        } => {
            current.revision += 1;
            current.wireguard_public_key = public_key;
            current.endpoints = vec![parse_endpoint(&endpoint)?];
            current.state = MembershipState::Active;
            Ok(vec![current])
        }
        Change::Replace {
            public_key,
            endpoint,
        } => {
            let tombstone = if current.state == MembershipState::Active {
                let mut tombstone = current.clone();
                tombstone.revision += 1;
                tombstone.state = MembershipState::Tombstoned;
                Some(tombstone)
            } else {
                None
            };
            current.owner_epoch += 1;
            current.revision = 1;
            current.wireguard_public_key = public_key;
            current.endpoints = vec![parse_endpoint(&endpoint)?];
            current.state = MembershipState::Active;
            Ok(tombstone.into_iter().chain([current]).collect())
        }
    }
}

fn parse_endpoint(endpoint: &str) -> anyhow::Result<SocketAddr> {
    endpoint
        .parse()
        .map_err(|_| anyhow::anyhow!("'{endpoint}' is not a valid IP:port endpoint"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiji_agent::membership::{MEMBERSHIP_PROTOCOL_VERSION, MEMBERSHIP_SCHEMA_VERSION};
    use std::net::Ipv4Addr;

    fn record() -> MembershipRecord {
        MembershipRecord {
            project_id: "demo".into(),
            recovery_epoch: 1,
            protocol_version: MEMBERSHIP_PROTOCOL_VERSION,
            schema_version: MEMBERSHIP_SCHEMA_VERSION,
            node_id: "node-a".into(),
            server_name: "node-a".into(),
            node_signing_public_key: vec![1; 32],
            wireguard_public_key: "old".into(),
            management_address: Ipv4Addr::new(100, 98, 64, 1),
            container_subnet: "198.18.1.0/24".into(),
            endpoints: vec!["192.0.2.1:51820".parse().unwrap()],
            owner_epoch: 1,
            revision: 4,
            state: MembershipState::Active,
        }
    }

    #[test]
    fn decommission_is_an_explicit_tombstone() {
        let changed = changed_records(record(), Change::Decommission).unwrap();
        assert_eq!(changed[0].revision, 5);
        assert_eq!(changed[0].state, MembershipState::Tombstoned);
    }

    #[test]
    fn replacement_fences_old_owner_before_new_epoch() {
        let changed = changed_records(
            record(),
            Change::Replace {
                public_key: "new".into(),
                endpoint: "198.51.100.1:52000".into(),
            },
        )
        .unwrap();
        assert_eq!(changed.len(), 2);
        assert_eq!(changed[0].state, MembershipState::Tombstoned);
        assert_eq!(changed[0].owner_epoch, 1);
        assert_eq!(changed[1].state, MembershipState::Active);
        assert_eq!(changed[1].owner_epoch, 2);
        assert_eq!(changed[1].revision, 1);
    }
}
