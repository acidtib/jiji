//! Membership changes: computed locally by the CLI and fanned out directly over SSH to every
//! configured server (`jiji-agent membership-import`), best-effort. There is no peer-to-peer
//! membership relay (see `jiji_agent::membership`'s module doc comment) -- a server that's
//! unreachable right now simply keeps its last-known membership until the next time any command
//! reaches it (`jiji server setup`, or a re-run of one of these commands).

use std::net::SocketAddr;
use std::path::Path;

use jiji_agent::membership::{MembershipRecord, MembershipScope, MembershipState, MembershipView};
use jiji_agent::AgentPaths;
use jiji_config::{validate_config, Config, Ssh};
use jiji_ssh::{SshPool, SshSession};
use jiji_tui::Ui;

use crate::ssh_adapter;

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
    let ssh = config
        .ssh
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No `ssh:` section is configured"))?;
    let recovery_epoch = crate::recovery_epoch::read(&path)?;
    let scope = MembershipScope::new(config.project.clone(), recovery_epoch);

    Ui::section("Publishing Mesh Membership:");
    let gathered = gather_membership(&config, ssh).await;
    let mut view = MembershipView::default();
    for record in gathered {
        // A stale/superseded record from a lagging peer is expected and harmless; only a
        // structural problem (wrong project, collision) is worth surfacing.
        if let Err(error) = view.apply(record, &scope) {
            Ui::warn(&format!(
                "Ignoring an inconsistent gathered record: {error}"
            ));
        }
    }
    let current = view.get(server).cloned().ok_or_else(|| {
        anyhow::anyhow!(
            "No reachable server has a membership record for '{server}'; run `jiji server setup` first"
        )
    })?;
    for record in changed_records(current, change)? {
        view.apply(record, &scope)?;
    }
    let records: Vec<MembershipRecord> = view.all().cloned().collect();

    let outcome = push_membership_everywhere(&config, ssh, &records).await?;
    for name in &outcome.reached {
        Ui::result_ok(name, "membership updated");
    }
    for (name, error) in &outcome.unreachable {
        Ui::result_warn(
            name,
            &format!("unreachable now, will catch up later: {error}"),
        );
    }
    if outcome.reached.is_empty() {
        anyhow::bail!("Could not reach any server to publish the membership change");
    }
    Ui::success("Membership updated on every reachable server.");
    Ok(())
}

pub(crate) struct MembershipPushOutcome {
    pub reached: Vec<String>,
    pub unreachable: Vec<(String, String)>,
}

/// Pushes the complete given membership set to every configured server concurrently
/// (bounded by `ssh.max_concurrent_starts`, same as every other multi-host fan-out in this
/// codebase), best-effort. Used both by this module's own commands and by `jiji server setup`
/// (which additionally needs newly enrolled servers to learn about every existing peer, and vice
/// versa).
pub(crate) async fn push_membership_everywhere(
    config: &Config,
    ssh: &Ssh,
    records: &[MembershipRecord],
) -> anyhow::Result<MembershipPushOutcome> {
    let mut names: Vec<String> = config.servers.keys().cloned().collect();
    names.sort();
    let mut connect_options = Vec::with_capacity(names.len());
    for name in &names {
        connect_options.push(ssh_adapter::connect_options(
            name,
            &config.servers[name],
            ssh,
        )?);
    }
    let project = config.project.clone();
    let records = records.to_vec();
    let pool = SshPool::new(ssh.max_concurrent_starts as usize);
    let operations: Vec<_> = connect_options
        .into_iter()
        .map(|options| {
            let project = project.clone();
            let records = records.clone();
            move || async move {
                let session = SshSession::connect(&options).await?;
                let result = push_membership(&session, &project, &records).await;
                session.close().await;
                result
            }
        })
        .collect();
    let results: Vec<anyhow::Result<()>> = pool.execute_concurrent(operations).await;

    let mut reached = Vec::new();
    let mut unreachable = Vec::new();
    for (name, result) in names.into_iter().zip(results) {
        match result {
            Ok(()) => reached.push(name),
            Err(error) => unreachable.push((name, error.to_string())),
        }
    }
    Ok(MembershipPushOutcome {
        reached,
        unreachable,
    })
}

pub(crate) async fn push_membership(
    session: &SshSession,
    project: &str,
    records: &[MembershipRecord],
) -> anyhow::Result<()> {
    let paths = AgentPaths::default_for_project(project);
    let update_path = paths.project_dir.join("membership-update.json");
    let write = session
        .execute_with_input(
            &format!("install -m 0600 /dev/stdin {}", update_path.display()),
            &serde_json::to_vec(records)?,
        )
        .await?;
    if !write.success {
        anyhow::bail!("could not stage membership update on {}", session.host());
    }
    let apply = session
        .execute(&format!(
            "{binary} membership-import --project {project} --state-dir {state} \
             --mesh-config {mesh} --input {update}",
            binary = paths.binary_path.display(),
            state = paths.state_dir.display(),
            mesh = paths.mesh_config_path.display(),
            update = update_path.display(),
        ))
        .await?;
    if !apply.success {
        anyhow::bail!(
            "{} rejected the membership update: {}",
            session.host(),
            apply.stderr.trim()
        );
    }
    Ok(())
}

/// Best-effort, concurrent collection of whatever membership every configured server currently
/// knows. A server that's unreachable, or that has never been enrolled yet, is silently skipped
/// -- the caller resolves conflicts (freshest revision wins) by replaying everything through a
/// `MembershipView`.
pub(crate) async fn gather_membership(config: &Config, ssh: &Ssh) -> Vec<MembershipRecord> {
    let connect_options: Vec<_> = config
        .servers
        .iter()
        .filter_map(|(name, server)| ssh_adapter::connect_options(name, server, ssh).ok())
        .collect();
    let project = config.project.clone();
    let pool = SshPool::new(ssh.max_concurrent_starts as usize);
    let operations: Vec<_> = connect_options
        .into_iter()
        .map(|options| {
            let project = project.clone();
            move || async move {
                let Ok(session) = SshSession::connect(&options).await else {
                    return Vec::new();
                };
                let result = pull_membership(&session, &project).await;
                session.close().await;
                result.unwrap_or_default()
            }
        })
        .collect();
    pool.execute_concurrent(operations)
        .await
        .into_iter()
        .flatten()
        .collect()
}

pub(crate) async fn pull_membership(
    session: &SshSession,
    project: &str,
) -> anyhow::Result<Vec<MembershipRecord>> {
    let paths = AgentPaths::default_for_project(project);
    let export = session
        .execute(&format!(
            "{} membership-export --state-dir {}",
            paths.binary_path.display(),
            paths.state_dir.display()
        ))
        .await?;
    if !export.success {
        anyhow::bail!(
            "could not read membership from {}: {}",
            session.host(),
            export.stderr.trim()
        );
    }
    Ok(serde_json::from_str(&export.stdout)?)
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
