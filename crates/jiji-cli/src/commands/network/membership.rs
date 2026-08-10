//! Membership changes: computed locally by the CLI from `jiji.yml` plus whatever a host reports
//! about itself, and fanned out directly over SSH to every configured server (`jiji-agent
//! membership-import`), best-effort. There is no peer-to-peer membership relay (see
//! `jiji_agent::membership`'s module doc comment) -- a server that's unreachable right now simply
//! keeps its last-known membership until the next time any command reaches it. There is no
//! operator-facing membership-editing command: `reconcile_record` and `compute_decommissions`
//! (used by `jiji server setup`) derive every membership change from config and observed host
//! state instead.

use std::collections::BTreeSet;

use jiji_agent::membership::{MembershipRecord, MembershipState, MembershipView};
use jiji_agent::AgentPaths;
use jiji_config::{Config, Ssh};
use jiji_ssh::{SshPool, SshSession};

use crate::ssh_adapter;

/// Reconciles a server's freshly observed WireGuard identity (public key + endpoint, read
/// straight off the host during `jiji server setup`) against its last known membership record
/// (`current`, `None` for a brand-new server). `candidate`'s `owner_epoch`/`revision`/`state` are
/// ignored -- this function decides them -- only its `wireguard_public_key`/`endpoints` (and the
/// identity/addressing fields that never change) are read. Returns the record to apply, or `None`
/// if nothing actually changed.
pub(crate) fn reconcile_record(
    current: Option<&MembershipRecord>,
    mut candidate: MembershipRecord,
) -> Option<MembershipRecord> {
    let Some(current) = current else {
        candidate.owner_epoch = 1;
        candidate.revision = 1;
        candidate.state = MembershipState::Active;
        return Some(candidate);
    };
    let key_changed = current.wireguard_public_key != candidate.wireguard_public_key;
    let endpoint_changed = current.endpoints != candidate.endpoints;
    let was_tombstoned = current.state == MembershipState::Tombstoned;
    if !key_changed && !endpoint_changed && !was_tombstoned {
        return None;
    }
    if key_changed || was_tombstoned {
        // A changed key is a new identity; re-enrolling a previously tombstoned node also needs
        // a new owner_epoch, since `MembershipView::apply` rejects resurrecting `Active` at the
        // same epoch a tombstone was published at. No separate tombstone message is needed to
        // fence the old owner_epoch out -- `MembershipView::apply`'s ordering rule already makes
        // a strictly higher owner_epoch win outright on every peer that receives this record,
        // and rejects any future record that still asserts the old, lower owner_epoch.
        candidate.owner_epoch = current.owner_epoch + 1;
        candidate.revision = 1;
    } else {
        candidate.owner_epoch = current.owner_epoch;
        candidate.revision = current.revision + 1;
    }
    candidate.state = MembershipState::Active;
    Some(candidate)
}

/// Tombstones every `Active` record in `view` whose `server_name` is no longer present in
/// `configured` -- i.e. a server removed from `servers:` in `jiji.yml`. Driven purely by full
/// membership in `configured`, never by which hosts a particular run could reach or targeted via
/// `-H`, so a server that's merely offline (or simply outside this run's `-H` filter) is never
/// mistaken for one that was deliberately removed from config.
pub(crate) fn compute_decommissions(
    configured: &BTreeSet<String>,
    view: &MembershipView,
) -> Vec<MembershipRecord> {
    view.all()
        .filter(|record| {
            record.state == MembershipState::Active && !configured.contains(&record.server_name)
        })
        .cloned()
        .map(|mut record| {
            record.revision += 1;
            record.state = MembershipState::Tombstoned;
            record
        })
        .collect()
}

pub(crate) struct MembershipPushOutcome {
    pub unreachable: Vec<(String, String)>,
}

/// Pushes the complete given membership set to every configured server except `excluded`
/// concurrently
/// (bounded by `ssh.max_concurrent_starts`, same as every other multi-host fan-out in this
/// codebase), best-effort. Used both by this module's own commands and by `jiji server setup`
/// after the setup targets already received the same records through their open install sessions.
pub(crate) async fn push_membership_everywhere(
    config: &Config,
    ssh: &Ssh,
    records: &[MembershipRecord],
    excluded: &BTreeSet<String>,
) -> anyhow::Result<MembershipPushOutcome> {
    let mut names: Vec<String> = config
        .servers
        .keys()
        .filter(|name| !excluded.contains(*name))
        .cloned()
        .collect();
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

    let mut unreachable = Vec::new();
    for (name, result) in names.into_iter().zip(results) {
        if let Err(error) = result {
            unreachable.push((name, error.to_string()));
        }
    }
    Ok(MembershipPushOutcome { unreachable })
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
            &format!("install -D -m 0600 /dev/stdin {}", update_path.display()),
            &serde_json::to_vec(records)?,
        )
        .await?;
    if !write.success {
        anyhow::bail!(
            "could not stage membership update on {}: {}",
            session.host(),
            write.stderr.trim()
        );
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

/// Best-effort, concurrent collection of whatever membership every non-excluded configured server
/// currently knows. A server that's unreachable, or that has never been enrolled yet, is silently
/// skipped -- the caller resolves conflicts (freshest revision wins) by replaying everything
/// through a `MembershipView`. Setup reads excluded targets through its already-open sessions.
pub(crate) async fn gather_membership(
    config: &Config,
    ssh: &Ssh,
    excluded: &BTreeSet<String>,
) -> Vec<MembershipRecord> {
    let connect_options: Vec<_> = config
        .servers
        .iter()
        .filter(|(name, _)| !excluded.contains(*name))
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

    fn candidate_with(key: &str, endpoint: &str) -> MembershipRecord {
        let mut candidate = record();
        candidate.wireguard_public_key = key.into();
        candidate.endpoints = vec![endpoint.parse().unwrap()];
        candidate
    }

    #[test]
    fn fresh_enroll_when_no_current_record() {
        let reconciled = reconcile_record(None, candidate_with("new", "192.0.2.1:51820")).unwrap();
        assert_eq!(reconciled.owner_epoch, 1);
        assert_eq!(reconciled.revision, 1);
        assert_eq!(reconciled.state, MembershipState::Active);
    }

    #[test]
    fn unchanged_key_and_endpoint_is_a_noop() {
        let current = record();
        let candidate = candidate_with("old", "192.0.2.1:51820");
        assert!(reconcile_record(Some(&current), candidate).is_none());
    }

    #[test]
    fn endpoint_only_change_bumps_revision_same_owner_epoch() {
        let current = record();
        let candidate = candidate_with("old", "198.51.100.1:52000");
        let reconciled = reconcile_record(Some(&current), candidate).unwrap();
        assert_eq!(reconciled.owner_epoch, 1);
        assert_eq!(reconciled.revision, 5);
        assert_eq!(reconciled.state, MembershipState::Active);
        assert_eq!(
            reconciled.endpoints,
            vec!["198.51.100.1:52000".parse().unwrap()]
        );
    }

    #[test]
    fn key_change_fences_a_new_owner_epoch() {
        let current = record();
        let candidate = candidate_with("new", "198.51.100.1:52000");
        let reconciled = reconcile_record(Some(&current), candidate).unwrap();
        assert_eq!(reconciled.state, MembershipState::Active);
        assert_eq!(reconciled.owner_epoch, 2);
        assert_eq!(reconciled.revision, 1);
        assert_eq!(reconciled.wireguard_public_key, "new");
    }

    #[test]
    fn resurrecting_a_tombstoned_node_fences_a_new_epoch() {
        let mut current = record();
        current.state = MembershipState::Tombstoned;
        let candidate = candidate_with("old", "192.0.2.1:51820");
        let reconciled = reconcile_record(Some(&current), candidate).unwrap();
        assert_eq!(reconciled.state, MembershipState::Active);
        assert_eq!(reconciled.owner_epoch, 2);
        assert_eq!(reconciled.revision, 1);
    }

    #[test]
    fn decommission_tombstones_only_servers_missing_from_config() {
        let mut view = MembershipView::default();
        let scope = jiji_agent::membership::MembershipScope::new("demo", 1);
        view.apply(record(), &scope).unwrap();
        let mut other = record();
        other.node_id = "node-b".into();
        other.server_name = "node-b".into();
        other.wireguard_public_key = "other".into();
        other.management_address = Ipv4Addr::new(100, 98, 64, 2);
        other.container_subnet = "198.18.2.0/24".into();
        view.apply(other, &scope).unwrap();

        let configured: BTreeSet<String> = ["node-a".to_string()].into_iter().collect();
        let decommissions = compute_decommissions(&configured, &view);
        assert_eq!(decommissions.len(), 1);
        assert_eq!(decommissions[0].server_name, "node-b");
        assert_eq!(decommissions[0].state, MembershipState::Tombstoned);
        assert_eq!(decommissions[0].revision, 5);
    }
}
