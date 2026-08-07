use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use jiji_ssh::{SshPool, SshSession};
use serde::{Deserialize, Serialize};

use crate::env_resolution::project_staging_dir;

static LOCK_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Root-owned, host-global path for the shared jiji-proxy/ingress lock. Co-located with the
/// existing `/etc/jiji/proxy-ingress/` state it guards (see `proxy_ingress::RULES_DIR`) rather
/// than a project-scoped, SSH-user-home-relative path: jiji-proxy and its ingress table are the
/// one resource multiple projects share on a host, so this lock carries no `{project}` segment.
pub const HOST_GLOBAL_PROXY_LOCK_PATH: &str = "/etc/jiji/proxy-ingress/lock";

fn project_maintenance_lock_path(project: &str) -> String {
    format!("{}/locks/maintenance.lock", project_staging_dir(project))
}

fn host_runtime_lock_path(project: &str) -> String {
    format!("{}/locks/host-runtime.lock", project_staging_dir(project))
}

fn service_scale_lock_path(project: &str, service: &str) -> String {
    format!(
        "{}/locks/service/{service}.lock",
        project_staging_dir(project)
    )
}

fn replica_lock_path(project: &str, replica_id: &str) -> String {
    format!(
        "{}/locks/replica/{replica_id}.lock",
        project_staging_dir(project)
    )
}

/// Everything after the last `/`, used to `mkdir -p` a lock's parent directory before creating it.
/// Plain string splitting rather than `std::path::Path` since these are always remote, forward-
/// slash POSIX paths regardless of which OS the CLI itself runs on.
fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/')
        .map(|(parent, _)| parent)
        .unwrap_or(".")
}

/// Which lock a given acquire/release/audit call is scoped to. `rank` is the deterministic
/// acquisition order every command sorts its full lock set by (`(rank, host, path)`) before
/// acquiring anything: since every command's lock set is a subset of this one fixed total order,
/// two commands can never form a cycle waiting on each other. Only `LogicalReplica` and
/// `HostGlobalProxy` locks are ever combined in one invocation today (a deploy/restart/rollback/
/// remove touching a shared-proxy ingress host).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockScope {
    ProjectMaintenance,
    HostRuntime,
    ServiceScale { service: String },
    LogicalReplica { replica_id: String },
    HostGlobalProxy,
}

impl LockScope {
    pub fn rank(&self) -> u8 {
        match self {
            LockScope::ProjectMaintenance => 0,
            LockScope::HostRuntime => 1,
            LockScope::ServiceScale { .. } => 2,
            LockScope::LogicalReplica { .. } => 3,
            LockScope::HostGlobalProxy => 4,
        }
    }

    pub fn lock_path(&self, project: &str) -> String {
        match self {
            LockScope::ProjectMaintenance => project_maintenance_lock_path(project),
            LockScope::HostRuntime => host_runtime_lock_path(project),
            LockScope::ServiceScale { service } => service_scale_lock_path(project, service),
            LockScope::LogicalReplica { replica_id } => replica_lock_path(project, replica_id),
            LockScope::HostGlobalProxy => HOST_GLOBAL_PROXY_LOCK_PATH.to_string(),
        }
    }
}

impl fmt::Display for LockScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LockScope::ProjectMaintenance => write!(f, "project-maintenance"),
            LockScope::HostRuntime => write!(f, "host-runtime"),
            LockScope::ServiceScale { service } => write!(f, "service-scale:{service}"),
            LockScope::LogicalReplica { replica_id } => write!(f, "replica:{replica_id}"),
            LockScope::HostGlobalProxy => write!(f, "proxy"),
        }
    }
}

/// One lock a command needs, on one host. A command computes its full set of these up front;
/// `OwnedDeploymentLocks::acquire` sorts the batch by `(scope.rank(), host, path)` and acquires
/// strictly in that order, concurrently within one rank.
#[derive(Debug, Clone)]
pub struct LockRequest {
    pub scope: LockScope,
    pub host: String,
}

impl LockRequest {
    pub fn new(scope: LockScope, host: impl Into<String>) -> Self {
        Self {
            scope,
            host: host.into(),
        }
    }
}

/// Sorts a batch of lock requests into deterministic acquisition order: ascending scope rank,
/// then host, then the scope's resolved path (a stable tiebreaker when two requests share a rank
/// and host, e.g. two `ServiceScale` requests for different services on the same host).
fn sort_requests(mut locks: Vec<LockRequest>, project: &str) -> Vec<LockRequest> {
    locks.sort_by(|a, b| {
        a.scope
            .rank()
            .cmp(&b.scope.rank())
            .then_with(|| a.host.cmp(&b.host))
            .then_with(|| a.scope.lock_path(project).cmp(&b.scope.lock_path(project)))
    });
    locks
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockInfo {
    pub message: String,
    pub acquired_at: u64,
    pub acquired_by: String,
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_id: Option<String>,
}

impl LockInfo {
    pub fn new(message: impl Into<String>) -> Self {
        let acquired_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after the unix epoch");
        let pid = std::process::id();
        Self {
            message: message.into(),
            acquired_at: acquired_at.as_secs(),
            acquired_by: current_user(),
            pid,
            lock_id: Some(format!(
                "{pid}-{}-{}",
                acquired_at.as_nanos(),
                LOCK_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            )),
        }
    }

    pub fn age_seconds(&self) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        now.saturating_sub(self.acquired_at)
    }
}

/// The local operator's username, used as `acquired_by` -- read locally rather than over SSH so
/// it reflects who ran `jiji lock acquire`, not the (often shared/service-account) SSH login user.
pub fn current_user() -> String {
    if let Ok(output) = std::process::Command::new("whoami").output() {
        if output.status.success() {
            let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !name.is_empty() {
                return name;
            }
        }
    }
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}

pub fn format_age(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m{}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h{}m", seconds / 3600, (seconds % 3600) / 60)
    }
}

/// Reads the lock at `path`, if any. Missing or malformed metadata is treated as unlocked so an
/// interrupted write can never permanently jam a command.
pub async fn read_lock(session: &SshSession, path: &str) -> anyhow::Result<Option<LockInfo>> {
    let result = session
        .execute(&format!("cat {path}/info.json 2>/dev/null || true"))
        .await?;
    let trimmed = result.stdout.trim();
    if let Ok(info) = serde_json::from_str::<LockInfo>(trimmed) {
        return Ok(Some(info));
    }
    Ok(None)
}

/// The inverse of `LockScope::lock_path`: recovers the scope a discovered `.../info.json` path
/// belongs to, so `jiji lock status`/`show` can list every lock present on a host without the
/// caller needing to already know what's there.
fn parse_scope_from_path(path: &str) -> Option<LockScope> {
    let stripped = path.strip_suffix("/info.json")?;
    if stripped == HOST_GLOBAL_PROXY_LOCK_PATH {
        return Some(LockScope::HostGlobalProxy);
    }
    if stripped.ends_with("/locks/maintenance.lock") {
        return Some(LockScope::ProjectMaintenance);
    }
    if stripped.ends_with("/locks/host-runtime.lock") {
        return Some(LockScope::HostRuntime);
    }
    if let Some(service) = stripped
        .rsplit_once("/locks/service/")
        .map(|(_, name)| name)
        .and_then(|name| name.strip_suffix(".lock"))
    {
        return Some(LockScope::ServiceScale {
            service: service.to_string(),
        });
    }
    if let Some(replica_id) = stripped
        .rsplit_once("/locks/replica/")
        .map(|(_, id)| id)
        .and_then(|id| id.strip_suffix(".lock"))
    {
        return Some(LockScope::LogicalReplica {
            replica_id: replica_id.to_string(),
        });
    }
    None
}

/// Discovers every lock currently present on `session` for `project`: every project-scoped lock
/// under `.jiji/{project}/locks/`, plus the host-global proxy lock (which has no project segment
/// but is still relevant context when inspecting a host). Missing/malformed entries are silently
/// skipped, the same resilience `read_lock` applies to a single lock.
pub async fn discover_locks(
    session: &SshSession,
    project: &str,
) -> anyhow::Result<Vec<(LockScope, LockInfo)>> {
    let dir = format!("{}/locks", project_staging_dir(project));
    let command = format!(
        "set -eu\n\
         if [ -d {dir} ]; then\n\
           find {dir} -name info.json 2>/dev/null | while IFS= read -r f; do\n\
             printf 'LOCKPATH:%s\\n' \"$f\"\n\
             cat \"$f\"\n\
             printf '\\n'\n\
           done\n\
         fi\n\
         if [ -f {proxy}/info.json ]; then\n\
           printf 'LOCKPATH:%s/info.json\\n' {proxy}\n\
           cat {proxy}/info.json\n\
           printf '\\n'\n\
         fi",
        proxy = HOST_GLOBAL_PROXY_LOCK_PATH,
    );
    let result = session.execute(&command).await?;

    let mut locks = Vec::new();
    let mut current_path: Option<String> = None;
    let mut current_body = String::new();
    let flush = |path: Option<String>, body: &str, locks: &mut Vec<(LockScope, LockInfo)>| {
        let Some(path) = path else { return };
        if let (Some(scope), Ok(info)) = (
            parse_scope_from_path(&path),
            serde_json::from_str::<LockInfo>(body.trim()),
        ) {
            locks.push((scope, info));
        }
    };
    for line in result.stdout.lines() {
        if let Some(path) = line.strip_prefix("LOCKPATH:") {
            flush(current_path.take(), &current_body, &mut locks);
            current_path = Some(path.to_string());
            current_body.clear();
        } else {
            current_body.push_str(line);
            current_body.push('\n');
        }
    }
    flush(current_path.take(), &current_body, &mut locks);
    Ok(locks)
}

#[derive(Debug)]
pub enum AcquireResult {
    Acquired,
    Held(Option<LockInfo>),
}

/// Claims the lock at `path` with `mkdir`, whose existence check and creation are one atomic
/// filesystem operation. Contention is reported through a stable stdout marker so it remains
/// distinguishable from transport and filesystem failures.
pub async fn acquire_lock(
    session: &SshSession,
    path: &str,
    info: &LockInfo,
) -> anyhow::Result<AcquireResult> {
    let content = serde_json::to_string_pretty(info)?;
    let lock_id = info.lock_id.as_deref().expect("new locks have an ID");
    let pending = format!("{path}.{lock_id}.pending");
    let parent = parent_dir(path);
    let command = format!(
        "set -eu\n\
         mkdir -p {parent}\n\
         mkdir {pending}\n\
         if ! install -m 0600 /dev/stdin {pending}/info.json; then rmdir {pending}; exit 74; fi\n\
         if ! mv -T {pending} {path} 2>/dev/null; then\n\
           rm -f {pending}/info.json\n\
           rmdir {pending}\n\
           echo JIJI_LOCK_HELD\n\
         fi"
    );
    let mut result = session
        .execute_with_input(&command, content.as_bytes())
        .await?;
    if result.success && result.stdout.trim() == "JIJI_LOCK_HELD" {
        if let Some(holder) = read_lock(session, path).await? {
            return Ok(AcquireResult::Held(Some(holder)));
        }

        recover_incomplete_lock(session, path).await?;
        result = session
            .execute_with_input(&command, content.as_bytes())
            .await?;
        if result.success && result.stdout.trim() == "JIJI_LOCK_HELD" {
            return Ok(AcquireResult::Held(read_lock(session, path).await?));
        }
    }
    if result.success {
        return Ok(AcquireResult::Acquired);
    }
    anyhow::bail!(
        "Could not acquire lock at {path} on {}: {}",
        session.host(),
        result.stderr.trim()
    );
}

async fn recover_incomplete_lock(session: &SshSession, path: &str) -> anyhow::Result<()> {
    let command = format!(
        "if [ -d {path} ]; then\n\
           rm -f {path}/info.json\n\
           rmdir {path} 2>/dev/null || true\n\
         elif [ -e {path} ]; then\n\
           rm -f {path}\n\
         fi"
    );
    let result = session.execute(&command).await?;
    if !result.success {
        anyhow::bail!(
            "Could not recover incomplete lock at {path} on {}: {}",
            session.host(),
            result.stderr.trim()
        );
    }
    Ok(())
}

/// Removes a lock only when its unique ID still belongs to the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseOwnedResult {
    Released,
    NoLongerOwned,
}

pub async fn release_owned_lock(
    session: &SshSession,
    path: &str,
    lock_id: &str,
) -> anyhow::Result<ReleaseOwnedResult> {
    let expected = serde_json::to_string(lock_id)?;
    let command = format!(
        "set -eu\n\
         test -d {path} || exit 75\n\
         actual=$(sed -n 's/.*\"lock_id\"[[:space:]]*:[[:space:]]*\"\\([^\"]*\\)\".*/\\1/p' {path}/info.json)\n\
         test \"$actual\" = {expected} || exit 75\n\
         rm -f {path}/info.json\n\
         rmdir {path}"
    );
    let result = session.execute(&command).await?;
    if result.code == Some(75) {
        return Ok(ReleaseOwnedResult::NoLongerOwned);
    }
    if !result.success {
        anyhow::bail!(
            "Could not release lock at {path} on {}: {}",
            session.host(),
            result.stderr.trim()
        );
    }
    Ok(ReleaseOwnedResult::Released)
}

pub async fn force_remove_lock(session: &SshSession, path: &str) -> anyhow::Result<()> {
    let command = format!(
        "if [ -d {path} ]; then rm -f {path}/info.json && rmdir {path}; else rm -f {path}; fi"
    );
    let result = session.execute(&command).await?;
    if !result.success {
        anyhow::bail!(
            "Could not remove lock at {path} on {}: {}",
            session.host(),
            result.stderr.trim()
        );
    }
    Ok(())
}

pub struct OwnedDeploymentLocks {
    project: String,
    lock_id: String,
    acquired: Vec<LockRequest>,
}

impl OwnedDeploymentLocks {
    pub async fn acquire(
        pool: &SshPool,
        sessions: &BTreeMap<String, Arc<SshSession>>,
        project: &str,
        locks: Vec<LockRequest>,
        message: impl Into<String>,
        timeout: u64,
        force: bool,
    ) -> anyhow::Result<Self> {
        let info = LockInfo::new(message);
        let lock_id = info
            .lock_id
            .as_deref()
            .expect("new locks have an ID")
            .to_string();
        let sorted = sort_requests(locks, project);
        let deadline = Instant::now() + Duration::from_secs(timeout);
        let mut warned = false;

        loop {
            if force {
                let operations: Vec<_> = sorted
                    .iter()
                    .map(|request| {
                        let session = sessions
                            .get(&request.host)
                            .expect("host has a session")
                            .clone();
                        let path = request.scope.lock_path(project);
                        move || async move { force_remove_lock(&session, &path).await }
                    })
                    .collect();
                for result in pool.execute_concurrent(operations).await {
                    result?;
                }
            }

            let mut acquired: Vec<LockRequest> = Vec::new();
            let mut failures: Vec<String> = Vec::new();
            let mut hard_failure = false;
            let mut rank_start = 0;
            while rank_start < sorted.len() && failures.is_empty() {
                let rank = sorted[rank_start].scope.rank();
                let mut rank_end = rank_start;
                while rank_end < sorted.len() && sorted[rank_end].scope.rank() == rank {
                    rank_end += 1;
                }
                let group = &sorted[rank_start..rank_end];
                let operations: Vec<_> = group
                    .iter()
                    .map(|request| {
                        let session = sessions
                            .get(&request.host)
                            .expect("host has a session")
                            .clone();
                        let path = request.scope.lock_path(project);
                        let info = info.clone();
                        move || async move { acquire_lock(&session, &path, &info).await }
                    })
                    .collect();
                let results = pool.execute_concurrent(operations).await;
                for (request, result) in group.iter().zip(results) {
                    match result {
                        Ok(AcquireResult::Acquired) => acquired.push(request.clone()),
                        Ok(AcquireResult::Held(Some(holder))) => failures.push(format!(
                            "{} on {}: \"{}\" by {} ({} ago)",
                            request.scope,
                            request.host,
                            holder.message,
                            holder.acquired_by,
                            format_age(holder.age_seconds())
                        )),
                        Ok(AcquireResult::Held(None)) => failures.push(format!(
                            "{} on {}: lock metadata is incomplete",
                            request.scope, request.host
                        )),
                        Err(error) => {
                            hard_failure = true;
                            failures
                                .push(format!("{} on {}: {error}", request.scope, request.host));
                        }
                    }
                }
                rank_start = rank_end;
            }

            if failures.is_empty() {
                return Ok(Self {
                    project: project.to_string(),
                    lock_id,
                    acquired,
                });
            }
            let (rollback_warnings, rollback_errors) =
                release_requests(pool, sessions, project, &lock_id, &acquired).await;
            let mut message = format!(
                "Could not acquire every lock this operation needs:\n  {}",
                failures.join("\n  ")
            );
            if !rollback_errors.is_empty() {
                message.push_str(&format!(
                    "\nCould not roll back partial locks:\n  {}",
                    rollback_errors.join("\n  ")
                ));
            }
            for warning in rollback_warnings {
                jiji_tui::Ui::warn(&warning);
            }
            if hard_failure || !rollback_errors.is_empty() || Instant::now() >= deadline {
                message.push_str(
                    "\nCheck `jiji lock status`, and once it is safe, run `jiji lock release`.",
                );
                anyhow::bail!("{message}");
            }
            if !warned {
                jiji_tui::Ui::warn(&format!(
                    "A lock is held; waiting up to {timeout}s for it to be released."
                ));
                warned = true;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    pub async fn release(
        self,
        pool: &SshPool,
        sessions: &BTreeMap<String, Arc<SshSession>>,
    ) -> anyhow::Result<()> {
        let (warnings, errors) =
            release_requests(pool, sessions, &self.project, &self.lock_id, &self.acquired).await;
        for warning in warnings {
            jiji_tui::Ui::warn(&warning);
        }
        if !errors.is_empty() {
            anyhow::bail!(
                "Could not release lock(s) on server(s): {}. Run `jiji lock release` after verifying no operation is active.",
                errors.join(", ")
            );
        }
        Ok(())
    }
}

async fn release_requests(
    pool: &SshPool,
    sessions: &BTreeMap<String, Arc<SshSession>>,
    project: &str,
    lock_id: &str,
    requests: &[LockRequest],
) -> (Vec<String>, Vec<String>) {
    let operations: Vec<_> = requests
        .iter()
        .map(|request| {
            let session = sessions
                .get(&request.host)
                .expect("acquired host has a session")
                .clone();
            let path = request.scope.lock_path(project);
            let lock_id = lock_id.to_string();
            move || async move { release_owned_lock(&session, &path, &lock_id).await }
        })
        .collect();
    let mut warnings = Vec::new();
    let mut errors = Vec::new();
    for (request, result) in requests
        .iter()
        .zip(pool.execute_concurrent(operations).await)
    {
        match result {
            Ok(ReleaseOwnedResult::Released) => {}
            Ok(ReleaseOwnedResult::NoLongerOwned) => warnings.push(format!(
                "{} lock on {} is no longer owned by this invocation; it was not removed.",
                request.scope, request.host
            )),
            Err(error) => errors.push(format!("{} on {}: {error}", request.scope, request.host)),
        }
    }
    (warnings, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_maintenance_lock_path_is_scoped_to_the_project() {
        assert_eq!(
            LockScope::ProjectMaintenance.lock_path("demo"),
            ".jiji/demo/locks/maintenance.lock"
        );
    }

    #[test]
    fn host_runtime_lock_path_is_scoped_to_the_project() {
        assert_eq!(
            LockScope::HostRuntime.lock_path("demo"),
            ".jiji/demo/locks/host-runtime.lock"
        );
    }

    #[test]
    fn service_scale_lock_path_is_scoped_to_project_and_service() {
        assert_eq!(
            LockScope::ServiceScale {
                service: "web".to_string()
            }
            .lock_path("demo"),
            ".jiji/demo/locks/service/web.lock"
        );
    }

    #[test]
    fn replica_lock_path_is_scoped_to_project_and_replica() {
        assert_eq!(
            LockScope::LogicalReplica {
                replica_id: "web-abc123".to_string()
            }
            .lock_path("demo"),
            ".jiji/demo/locks/replica/web-abc123.lock"
        );
    }

    #[test]
    fn host_global_proxy_lock_path_has_no_project_segment() {
        assert_eq!(
            LockScope::HostGlobalProxy.lock_path("demo"),
            "/etc/jiji/proxy-ingress/lock"
        );
        assert_eq!(
            LockScope::HostGlobalProxy.lock_path("other-project"),
            LockScope::HostGlobalProxy.lock_path("demo")
        );
    }

    #[test]
    fn rank_orders_scopes_from_project_maintenance_to_proxy() {
        assert!(LockScope::ProjectMaintenance.rank() < LockScope::HostRuntime.rank());
        assert!(
            LockScope::HostRuntime.rank()
                < LockScope::ServiceScale {
                    service: "web".to_string()
                }
                .rank()
        );
        assert!(
            LockScope::ServiceScale {
                service: "web".to_string()
            }
            .rank()
                < LockScope::LogicalReplica {
                    replica_id: "web-abc123".to_string()
                }
                .rank()
        );
        assert!(
            LockScope::LogicalReplica {
                replica_id: "web-abc123".to_string()
            }
            .rank()
                < LockScope::HostGlobalProxy.rank()
        );
    }

    #[test]
    fn display_renders_audit_friendly_labels() {
        assert_eq!(
            LockScope::ProjectMaintenance.to_string(),
            "project-maintenance"
        );
        assert_eq!(LockScope::HostRuntime.to_string(), "host-runtime");
        assert_eq!(
            LockScope::ServiceScale {
                service: "web".to_string()
            }
            .to_string(),
            "service-scale:web"
        );
        assert_eq!(
            LockScope::LogicalReplica {
                replica_id: "web-abc123".to_string()
            }
            .to_string(),
            "replica:web-abc123"
        );
        assert_eq!(LockScope::HostGlobalProxy.to_string(), "proxy");
    }

    #[test]
    fn sort_requests_orders_by_rank_then_host_then_path() {
        let locks = vec![
            LockRequest::new(LockScope::HostGlobalProxy, "node-b"),
            LockRequest::new(
                LockScope::LogicalReplica {
                    replica_id: "web-2".to_string(),
                },
                "node-a",
            ),
            LockRequest::new(
                LockScope::LogicalReplica {
                    replica_id: "web-1".to_string(),
                },
                "node-a",
            ),
            LockRequest::new(LockScope::ProjectMaintenance, "node-b"),
            LockRequest::new(LockScope::HostRuntime, "node-a"),
        ];
        let sorted = sort_requests(locks, "demo");
        let labels: Vec<String> = sorted
            .iter()
            .map(|request| format!("{}@{}", request.scope, request.host))
            .collect();
        assert_eq!(
            labels,
            vec![
                "project-maintenance@node-b",
                "host-runtime@node-a",
                "replica:web-1@node-a",
                "replica:web-2@node-a",
                "proxy@node-b",
            ]
        );
    }

    #[test]
    fn parent_dir_splits_on_the_last_slash() {
        assert_eq!(
            parent_dir(".jiji/demo/locks/replica/web-1.lock"),
            ".jiji/demo/locks/replica"
        );
        assert_eq!(
            parent_dir("/etc/jiji/proxy-ingress/lock"),
            "/etc/jiji/proxy-ingress"
        );
        assert_eq!(parent_dir("nofile"), ".");
    }

    #[test]
    fn parse_scope_from_path_round_trips_lock_path_for_every_scope() {
        let scopes = [
            LockScope::ProjectMaintenance,
            LockScope::HostRuntime,
            LockScope::ServiceScale {
                service: "web".to_string(),
            },
            LockScope::LogicalReplica {
                replica_id: "web-abc123".to_string(),
            },
            LockScope::HostGlobalProxy,
        ];
        for scope in scopes {
            let path = format!("{}/info.json", scope.lock_path("demo"));
            assert_eq!(parse_scope_from_path(&path), Some(scope.clone()), "{scope}");
        }
    }

    #[test]
    fn parse_scope_from_path_rejects_an_unrecognized_path() {
        assert_eq!(
            parse_scope_from_path(".jiji/demo/deploy.lock/info.json"),
            None
        );
    }

    #[test]
    fn age_seconds_is_zero_for_a_lock_acquired_just_now() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let info = LockInfo {
            message: "test".to_string(),
            acquired_at: now,
            acquired_by: "alice".to_string(),
            pid: 1,
            lock_id: None,
        };
        assert_eq!(info.age_seconds(), 0);
    }

    #[test]
    fn format_age_covers_seconds_minutes_and_hours() {
        assert_eq!(format_age(5), "5s");
        assert_eq!(format_age(65), "1m5s");
        assert_eq!(format_age(3725), "1h2m");
    }

    #[test]
    fn legacy_lock_deserializes_without_an_ownership_id() {
        let info: LockInfo = serde_json::from_str(
            r#"{"message":"maintenance","acquired_at":1,"acquired_by":"alice","pid":2}"#,
        )
        .unwrap();
        assert_eq!(info.lock_id, None);
    }

    #[test]
    fn new_locks_have_distinct_ownership_ids() {
        let first = LockInfo::new("first");
        let second = LockInfo::new("second");
        assert!(first.lock_id.is_some());
        assert_ne!(first.lock_id, second.lock_id);
    }
}
