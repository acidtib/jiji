use jiji_tui::Ui;

use super::{close_all, connect_targets, read_all};
use crate::audit::{self, AuditStatus};
use crate::lock::{self, LockScope};

/// Parses `--replica`/`--service`/`--scope` (mutually exclusive, enforced by clap) into the
/// `LockScope` to release, defaulting to the project-maintenance lock `jiji lock` has always
/// targeted when none are given.
fn resolve_scope(
    replica: Option<&str>,
    service: Option<&str>,
    scope: Option<&str>,
) -> anyhow::Result<LockScope> {
    if let Some(replica_id) = replica {
        return Ok(LockScope::LogicalReplica {
            replica_id: replica_id.to_string(),
        });
    }
    if let Some(service) = service {
        return Ok(LockScope::ServiceScale {
            service: service.to_string(),
        });
    }
    if let Some(scope) = scope {
        return match scope {
            "host-runtime" => Ok(LockScope::HostRuntime),
            "proxy" => Ok(LockScope::HostGlobalProxy),
            other => anyhow::bail!(
                "Unknown --scope '{other}'. Use 'host-runtime' or 'proxy' (or --replica/--service for those scopes)."
            ),
        };
    }
    Ok(LockScope::ProjectMaintenance)
}

pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    hosts: Option<&str>,
    services: Option<&str>,
    replica: Option<&str>,
    service: Option<&str>,
    scope: Option<&str>,
) -> anyhow::Result<()> {
    Ui::section("Lock Release:");
    let started_at = std::time::Instant::now();
    let target_scope = resolve_scope(replica, service, scope)?;
    let explicit_scope = replica.is_some() || service.is_some() || scope.is_some();

    Ui::section("Connecting:");
    let targets = connect_targets(environment, config_file, hosts, services, false).await?;

    if !explicit_scope {
        Ui::section("Checking Existing Locks:");
        let statuses = read_all(&targets).await?;
        let locked_count = statuses.iter().filter(|(_, info)| info.is_some()).count();
        if locked_count == 0 {
            Ui::warn("No deployment locks found.");
            close_all(&targets.sessions).await;
            return Ok(());
        }
    }

    Ui::section("Removing Lock Files:");
    Ui::say(&format!("Scope: {target_scope}"), 1);
    let names: Vec<String> = targets.sessions.keys().cloned().collect();
    let rel_progress =
        jiji_tui::ServerSetupProgress::with_title(names.clone(), "Releasing".to_string());
    let rel_handle = rel_progress.handle();
    for h in &names {
        rel_handle.set_status(h, "releasing");
    }
    let operations: Vec<_> = names
        .iter()
        .map(|name| targets.sessions.get(name).expect("connected above").clone())
        .map(|session| {
            let path = target_scope.lock_path(&targets.project);
            move || async move { lock::force_remove_lock(&session, &path).await }
        })
        .collect();
    let results = targets.pool.execute_concurrent(operations).await;

    let mut failures = Vec::new();
    for (name, result) in names.iter().zip(results) {
        match result {
            Ok(()) => {
                rel_handle.mark_success(name, "released");
                Ui::result_ok(name, "lock released");
                let session = targets.sessions.get(name).expect("connected above");
                audit::record(
                    session,
                    &targets.project,
                    "lock_release",
                    AuditStatus::Success,
                    format!("released by {}", lock::current_user()),
                    Some(&target_scope.to_string()),
                    None,
                    Some(started_at.elapsed()),
                )
                .await;
            }
            Err(error) => {
                rel_handle.mark_failed(name, &error.to_string());
                Ui::result_error(name, &error.to_string());
                failures.push(name.clone());
            }
        }
    }
    rel_progress.finish();
    close_all(&targets.sessions).await;

    if !failures.is_empty() {
        anyhow::bail!(
            "Could not release lock on server(s): {}. Retry `jiji lock release` for those hosts.",
            failures.join(", ")
        );
    }

    Ui::success("\nDeployment lock released.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_project_maintenance_when_nothing_is_given() {
        assert_eq!(
            resolve_scope(None, None, None).unwrap(),
            LockScope::ProjectMaintenance
        );
    }

    #[test]
    fn replica_flag_selects_logical_replica_scope() {
        assert_eq!(
            resolve_scope(Some("web-abc123"), None, None).unwrap(),
            LockScope::LogicalReplica {
                replica_id: "web-abc123".to_string()
            }
        );
    }

    #[test]
    fn service_flag_selects_service_scale_scope() {
        assert_eq!(
            resolve_scope(None, Some("web"), None).unwrap(),
            LockScope::ServiceScale {
                service: "web".to_string()
            }
        );
    }

    #[test]
    fn scope_flag_accepts_host_runtime_and_proxy() {
        assert_eq!(
            resolve_scope(None, None, Some("host-runtime")).unwrap(),
            LockScope::HostRuntime
        );
        assert_eq!(
            resolve_scope(None, None, Some("proxy")).unwrap(),
            LockScope::HostGlobalProxy
        );
    }

    #[test]
    fn scope_flag_rejects_an_unknown_value() {
        assert!(resolve_scope(None, None, Some("bogus")).is_err());
    }
}
