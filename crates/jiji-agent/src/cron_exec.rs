//! Runs a claimed cron run's one-off container locally (`plans/service-cron.md`'s "Execution
//! Model"). Unlike a service deployment (rendered by `jiji-cli` and started over SSH), a cron run
//! is started by this agent process itself: the scheduler (a later phase) and `CronRun`'s manual
//! path (`api.rs`) both claim a run, lease an address, then hand off here.
//!
//! A cron run's own `run_id` plays the role a service deployment's `deployment_id` plays: unique
//! per container start, immutable for the run's full lifetime (invariant 9), and the key
//! `AddressAllocator` leases its address under (`leases::cron_replica_id` supplies the paired
//! synthetic `replica_id`). There is no separate `deployment_id` concept for a cron run.
//!
//! Detached (`--detach`), not a direct child of this process: a container's lifecycle must
//! survive an agent crash/restart (the plan's "Agent restart" failure semantics --
//! `recover_claimed_runs` below resumes observation of it rather than starting a replacement),
//! which a foreground child process could not do. This agent instead stays "attached" by blocking
//! on `wait`, exactly like `docker/podman wait <name>` was designed for.

use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jiji_network::NetworkedContainerRun;
use tracing::warn;

use crate::cron::{CronJobSpec, CronRun, CronRunState};
use crate::engine::Engine;
use crate::leases::DEFAULT_QUARANTINE_SECONDS;
use crate::store::AgentStore;

/// Conservative bound under Docker's (128) and Podman's (~250, but undocumented) own container
/// name limits.
const MAX_CONTAINER_NAME_LEN: usize = 120;
/// After `stop`'s own grace period elapses without the container exiting, how much longer this
/// agent waits before giving up and force-removing it (the plan's "Timeout" failure semantics).
const STOP_GRACE_SECS: u64 = 30;

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// `<project>-<service>-cron-<cron-slug>-<first-12-run-id-chars>`, with a hash-suffix fallback
/// when that exceeds the engine's name limit (project/service/cron names are already validated
/// DNS-safe ASCII by `jiji_config::validation`, so byte-index truncation is always char-safe).
pub fn cron_container_name(project: &str, service: &str, cron_name: &str, run_id: &str) -> String {
    let suffix = run_id.get(..12).unwrap_or(run_id);
    let readable = format!("{project}-{service}-cron-{cron_name}-{suffix}");
    if readable.len() <= MAX_CONTAINER_NAME_LEN {
        return readable;
    }
    let hash = short_hash(&readable);
    let keep = MAX_CONTAINER_NAME_LEN.saturating_sub(hash.len() + 1);
    format!("{}-{}", &readable[..keep], hash)
}

fn short_hash(value: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(value.as_bytes())[..4]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Exactly the label set the plan's "Execution Model" section specifies -- a cron container's own
/// `jiji.deployment` is its `run_id` (see this module's doc comment), not `spec.source_deployment_id`
/// (which names the *service* deployment the spec was rendered from, a different concept).
fn cron_container_labels(spec: &CronJobSpec, run_id: &str, address: Ipv4Addr) -> Vec<String> {
    let pairs = [
        ("jiji.managed", "true".to_string()),
        ("jiji.project", spec.project.clone()),
        ("jiji.service", spec.service.clone()),
        ("jiji.server", spec.server.clone()),
        ("jiji.resource", "cron".to_string()),
        ("jiji.cron", spec.cron_name.clone()),
        ("jiji.cron-run", run_id.to_string()),
        ("jiji.deployment", run_id.to_string()),
        ("jiji.lease", address.to_string()),
    ];
    let mut labels = Vec::with_capacity(pairs.len() * 2);
    for (key, value) in pairs {
        labels.push("--label".to_string());
        labels.push(format!("{key}={value}"));
    }
    labels
}

fn to_container_engine(engine: Engine) -> jiji_config::ContainerEngine {
    match engine {
        Engine::Docker => jiji_config::ContainerEngine::Docker,
        Engine::Podman => jiji_config::ContainerEngine::Podman,
    }
}

/// Builds the full run: reuses `jiji_network::NetworkedContainerRun` (the same renderer
/// `jiji-cli`'s `container_runtime::build_dynamic_run` uses for a service deployment) for the
/// network/address/DNS portion, since a cron container inherits the project bridge and DNS
/// exactly like a service container does. Everything else (mounts, resources, env file, command)
/// was already rendered by `jiji-cli` into `spec`'s fields at `CronSpecApply` time.
fn render_cron_run(
    spec: &CronJobSpec,
    engine: Engine,
    run_id: &str,
    container_name: &str,
    address: Ipv4Addr,
) -> Result<NetworkedContainerRun, String> {
    let dns_address: Ipv4Addr = spec.dns_address.parse().map_err(|error| {
        format!(
            "cron spec dns_address '{}' is not a valid IPv4 address: {error}",
            spec.dns_address
        )
    })?;
    let mut run = NetworkedContainerRun {
        engine: to_container_engine(engine),
        container_name: container_name.to_string(),
        image: spec.image.clone(),
        address,
        dns_address,
        bridge_name: spec.bridge_network.clone(),
        // Unused by `.args()` (only `bridge_name` is); no cron equivalent field exists to fill it
        // from, since nothing about cron execution needs the project's kernel bridge device name.
        bridge_interface: String::new(),
        // A cron job is unconditionally rejected at config-validation time for a service using
        // `network_mode: service:<name>`, so a cron container always gets its own address.
        shared_with_container: None,
        extra_args: Vec::new(),
        command: spec.command.clone(),
    };
    run.extra_args.push("--detach".to_string());
    run.extra_args.push("--restart".to_string());
    run.extra_args.push("no".to_string());
    run.extra_args
        .extend(cron_container_labels(spec, run_id, address));
    run.extra_args.extend(spec.mount_args.iter().cloned());
    run.extra_args.push("--env-file".to_string());
    run.extra_args.push(spec.env_file_path.clone());
    run.extra_args.extend(spec.resource_args.iter().cloned());
    Ok(run)
}

/// Takes the engine binary as an explicit path/name (never `engine.as_str()` internally),
/// mirroring `discovery.rs`'s `discover`/`discover_with_binary` split: it lets tests point at a
/// fake script instead of mutating the process-wide `PATH` (unsafe under parallel test threads).
async fn run_engine(binary: &str, args: &[&str]) -> Result<String, String> {
    let output = tokio::process::Command::new(binary)
        .args(args)
        .output()
        .await
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Blocks until the container exits, returning its exit code. Never returns a "missing" status
/// the way an SSH channel can (see `jiji-ssh`'s `ChannelMsg::ExitStatus` gotcha): `wait` is a
/// local `waitpid`-backed engine command, which always observes a real exit code.
async fn wait_for_exit(binary: &str, container_name: &str) -> Result<i32, String> {
    run_engine(binary, &["wait", container_name])
        .await?
        .parse::<i32>()
        .map_err(|error| format!("engine reported a non-numeric exit code: {error}"))
}

fn release_address(store: &AgentStore, run_id: &str, timestamp: u64) {
    if let Err(error) =
        store.quarantine_address_lease(run_id, timestamp.saturating_add(DEFAULT_QUARANTINE_SECONDS))
    {
        warn!(%error, run_id, "failed to release a cron run's address lease");
    }
}

fn finish(
    store: &Mutex<AgentStore>,
    run_id: &str,
    state: CronRunState,
    exit_code: Option<i32>,
    error: Option<String>,
) {
    let timestamp = now_secs();
    match store.lock() {
        Ok(store) => {
            release_address(&store, run_id, timestamp);
            if let Err(error) = store.finish_cron_run(run_id, state, timestamp, exit_code, error) {
                warn!(%error, run_id, "failed to record a cron run's final result");
            }
        }
        Err(_) => warn!(
            run_id,
            "local store lock poisoned; could not finalize cron run"
        ),
    }
}

/// Starts a just-claimed run's container and drives it to completion, updating durable state at
/// each transition so a concurrent agent restart never loses track of it. Spawned as its own task
/// by the caller (`api.rs`'s `CronRun` handler, or a later phase's scheduler) -- never awaited
/// inline, since `CronRun` returns as soon as the claim (not the run) succeeds.
pub async fn execute_claimed_run(
    store: Arc<Mutex<AgentStore>>,
    engine: Engine,
    spec: CronJobSpec,
    run: CronRun,
    address: Ipv4Addr,
) {
    execute_claimed_run_with_binary(engine.as_str(), store, engine, spec, run, address).await;
}

async fn execute_claimed_run_with_binary(
    binary: &str,
    store: Arc<Mutex<AgentStore>>,
    engine: Engine,
    spec: CronJobSpec,
    run: CronRun,
    address: Ipv4Addr,
) {
    let container_name =
        cron_container_name(&spec.project, &spec.service, &spec.cron_name, &run.run_id);
    let run_command = match render_cron_run(&spec, engine, &run.run_id, &container_name, address) {
        Ok(run_command) => run_command,
        Err(error) => {
            finish(&store, &run.run_id, CronRunState::Failed, None, Some(error));
            return;
        }
    };
    // `args()[0]` is `engine.to_string()` (`"docker"`/`"podman"`), used only to pick a binary
    // above -- `binary` (possibly a fake test script) replaces it here, and the rest of the argv
    // never repeats it.
    let args = run_command.args();
    let start = tokio::process::Command::new(binary)
        .args(&args[1..])
        .output()
        .await;
    match &start {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
            // `run` combines create+start; a start failure after a successful create can leave a
            // dead container occupying the name. Best-effort cleanup so a retried run never
            // collides with it.
            let _ = run_engine(binary, &["rm", "-f", &container_name]).await;
            finish(
                &store,
                &run.run_id,
                CronRunState::Failed,
                None,
                Some(format!("could not start cron container: {detail}")),
            );
            return;
        }
        Err(error) => {
            finish(
                &store,
                &run.run_id,
                CronRunState::Failed,
                None,
                Some(format!("could not start cron container: {error}")),
            );
            return;
        }
    }

    let started_at = now_secs();
    let start_recorded = match store.lock() {
        Ok(store) => store
            .start_cron_run(
                &run.run_id,
                started_at,
                &run.run_id,
                &container_name,
                &address.to_string(),
            )
            .unwrap_or(false),
        Err(_) => false,
    };
    if !start_recorded {
        warn!(run_id = %run.run_id, "could not record cron run start; continuing to monitor the container anyway");
    }

    let remaining = Duration::from_secs(spec.timeout_seconds);
    monitor_and_finish(binary, store, &run.run_id, &container_name, remaining).await;
}

/// Blocks (up to `remaining`) until the container exits, then persists the final result --
/// shared by a freshly started run and `recover_claimed_runs`'s resumption of one already running
/// when this agent last restarted (which computes `remaining` as whatever was left of the
/// original timeout, not the full configured duration again).
async fn monitor_and_finish(
    binary: &str,
    store: Arc<Mutex<AgentStore>>,
    run_id: &str,
    container_name: &str,
    remaining: Duration,
) {
    let (state, exit_code, error) =
        match tokio::time::timeout(remaining, wait_for_exit(binary, container_name)).await {
            Ok(Ok(code)) if code == 0 => (CronRunState::Succeeded, Some(code), None),
            Ok(Ok(code)) => (CronRunState::Failed, Some(code), None),
            Ok(Err(detail)) => (CronRunState::Failed, None, Some(detail)),
            Err(_elapsed) => {
                let _ = run_engine(binary, &["stop", container_name]).await;
                let exit_code = match tokio::time::timeout(
                    Duration::from_secs(STOP_GRACE_SECS),
                    wait_for_exit(binary, container_name),
                )
                .await
                {
                    Ok(Ok(code)) => Some(code),
                    _ => {
                        let _ = run_engine(binary, &["rm", "-f", container_name]).await;
                        None
                    }
                };
                (
                    CronRunState::TimedOut,
                    exit_code,
                    Some(format!(
                        "exceeded its configured timeout ({}s remaining when last observed)",
                        remaining.as_secs()
                    )),
                )
            }
        };
    finish(&store, run_id, state, exit_code, error);
}

/// Fallback used only when a run's installed spec was removed (`CronSpecRemove`) while it was
/// still active across an agent restart -- the plan's "Specification removal" failure semantics
/// require the run to keep completing on its own stored context, but that context no longer
/// includes a timeout once the spec is gone. Matches `jiji_config`'s own default (`1h`).
const FALLBACK_TIMEOUT_SECS: u64 = 3600;

struct CronContainerObservation {
    name: String,
    run_id: Option<String>,
    running: bool,
}

async fn list_cron_containers(
    binary: &str,
    engine: Engine,
    project: &str,
) -> Result<Vec<CronContainerObservation>, String> {
    let template = format!(
        "{{{{.Names}}}}|{}|{{{{.State}}}}",
        crate::discovery::label_template(engine, "jiji.cron-run"),
    );
    let output = tokio::process::Command::new(binary)
        .args(["ps", "-a"])
        .args(["--filter", "label=jiji.managed=true"])
        .args(["--filter", &format!("label=jiji.project={project}")])
        .args(["--filter", "label=jiji.resource=cron"])
        .args(["--format", &template])
        .output()
        .await
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let mut fields = line.split('|');
            let name = fields.next()?.trim().to_string();
            if name.is_empty() {
                return None;
            }
            let run_id = fields
                .next()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            let running = fields.next().unwrap_or_default().trim() == "running";
            Some(CronContainerObservation {
                name,
                run_id,
                running,
            })
        })
        .collect())
}

async fn inspect_exit_code(binary: &str, container_name: &str) -> Option<i32> {
    run_engine(
        binary,
        &["inspect", container_name, "--format", "{{.State.ExitCode}}"],
    )
    .await
    .ok()?
    .parse()
    .ok()
}

fn remaining_timeout(run: &CronRun, timeout_seconds: u64, now: u64) -> Duration {
    let began_at = run.started_at.unwrap_or(run.claimed_at);
    let deadline = began_at.saturating_add(timeout_seconds);
    Duration::from_secs(deadline.saturating_sub(now))
}

/// On startup, reconciles every `claimed`/`running` cron run against actual local containers
/// before the agent accepts a due or manual run (the plan's "Address Leases and Networking" and
/// "Failure Semantics: Agent restart" sections):
/// - a matching running container resumes observation (a background task, not a blocking one --
///   this function must return promptly so startup isn't held hostage by a long-running job);
/// - a matching exited container is finalized from its real exit code;
/// - an active run with no matching container becomes `failed` and releases its address;
/// - an unclaimed managed cron container (no active run references it -- e.g. one a prior agent
///   instance finished but never got to remove) is stopped and force-removed.
pub async fn recover_claimed_runs(store: Arc<Mutex<AgentStore>>, engine: Engine, project: &str) {
    recover_claimed_runs_with_binary(engine.as_str(), store, engine, project).await;
}

async fn recover_claimed_runs_with_binary(
    binary: &str,
    store: Arc<Mutex<AgentStore>>,
    engine: Engine,
    project: &str,
) {
    let containers = match list_cron_containers(binary, engine, project).await {
        Ok(containers) => containers,
        Err(error) => {
            warn!(
                %error,
                "cron container recovery: could not list containers; starting without recovering active runs"
            );
            return;
        }
    };
    let active_runs: Vec<CronRun> = match store.lock() {
        Ok(store) => store
            .cron_runs(&crate::cron::CronRunFilter::default())
            .unwrap_or_default()
            .into_iter()
            .filter(|run| run.state.is_active())
            .collect(),
        Err(_) => {
            warn!("local store lock poisoned; skipping cron run recovery");
            return;
        }
    };

    let now = now_secs();
    let mut matched_run_ids = std::collections::BTreeSet::new();
    for run in &active_runs {
        let container = containers
            .iter()
            .find(|container| container.run_id.as_deref() == Some(run.run_id.as_str()));
        let Some(container) = container else {
            finish(
                &store,
                &run.run_id,
                CronRunState::Failed,
                None,
                Some("cron container missing on agent restart".to_string()),
            );
            continue;
        };
        matched_run_ids.insert(run.run_id.clone());

        if !container.running {
            let exit_code = inspect_exit_code(binary, &container.name).await;
            let state = if exit_code == Some(0) {
                CronRunState::Succeeded
            } else {
                CronRunState::Failed
            };
            finish(&store, &run.run_id, state, exit_code, None);
            continue;
        }

        let timeout_seconds = match store.lock() {
            Ok(store) => store
                .cron_spec(&run.service, &run.cron_name)
                .ok()
                .flatten()
                .map(|spec| spec.timeout_seconds),
            Err(_) => None,
        }
        .unwrap_or(FALLBACK_TIMEOUT_SECS);
        let remaining = remaining_timeout(run, timeout_seconds, now);

        let binary_owned = binary.to_string();
        let run_id = run.run_id.clone();
        let container_name = container.name.clone();
        let store = Arc::clone(&store);
        tokio::spawn(async move {
            monitor_and_finish(&binary_owned, store, &run_id, &container_name, remaining).await;
        });
    }

    for container in &containers {
        let matched = container
            .run_id
            .as_deref()
            .is_some_and(|run_id| matched_run_ids.contains(run_id));
        if matched {
            continue;
        }
        warn!(
            container = %container.name,
            "removing unclaimed cron container found on agent restart"
        );
        let _ = run_engine(binary, &["stop", &container.name]).await;
        let _ = run_engine(binary, &["rm", "-f", &container.name]).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> CronJobSpec {
        CronJobSpec {
            project: "demo".into(),
            service: "twitch".into(),
            cron_name: "sync-twitch".into(),
            revision: 1,
            canonical_hash: "hash".into(),
            owner_node_id: "node-a".into(),
            owner_epoch: 1,
            server: "app-1".into(),
            source_deployment_id: "dep-a".into(),
            source_replica_id: "replica-a".into(),
            image: "ghcr.io/example/twitch-sync:latest".into(),
            schedule: "7 */2 * * *".into(),
            timezone: "UTC".into(),
            timeout_seconds: 3600,
            overlap: crate::cron::CronOverlap::Forbid,
            missed_runs: crate::cron::CronMissedRuns::Skip,
            command: vec!["npm".into(), "run".into(), "sync:twitch".into()],
            env_file_path: "/var/lib/jiji/demo/env/twitch".into(),
            mount_args: vec!["-v".into(), "twitch-data:/data".into()],
            resource_args: vec!["--memory".into(), "512m".into()],
            bridge_network: "jiji-demo".into(),
            dns_address: "100.64.0.5".into(),
        }
    }

    #[test]
    fn container_name_uses_the_first_twelve_run_id_characters() {
        let name = cron_container_name("demo", "twitch", "sync-twitch", "abcdef0123456789");
        assert_eq!(name, "demo-twitch-cron-sync-twitch-abcdef012345");
    }

    #[test]
    fn container_name_falls_back_to_a_hash_suffix_when_too_long() {
        // Worst-case-length project/service/cron names (each individually valid under
        // `jiji_config::validation`'s 63-character cap) together exceed the engine limit.
        let long_project = "p".repeat(63);
        let long_service = "s".repeat(63);
        let long_cron_name = "c".repeat(63);
        let readable = format!("{long_project}-{long_service}-cron-{long_cron_name}-abcdef012345");
        assert!(readable.len() > MAX_CONTAINER_NAME_LEN);

        let name = cron_container_name(
            &long_project,
            &long_service,
            &long_cron_name,
            "abcdef0123456789",
        );
        assert!(name.len() <= MAX_CONTAINER_NAME_LEN);
        assert!(name.ends_with(&short_hash(&readable)));
    }

    #[test]
    fn render_cron_run_produces_the_full_label_set_and_no_shared_namespace() {
        let run = render_cron_run(
            &spec(),
            Engine::Docker,
            "run-1",
            "demo-twitch-cron-sync-twitch-run1",
            "100.64.0.9".parse().unwrap(),
        )
        .unwrap();
        assert!(run.shared_with_container.is_none());
        let args = run.args();
        let joined = args.join(" ");
        for expected in [
            "--label jiji.managed=true",
            "--label jiji.project=demo",
            "--label jiji.service=twitch",
            "--label jiji.server=app-1",
            "--label jiji.resource=cron",
            "--label jiji.cron=sync-twitch",
            "--label jiji.cron-run=run-1",
            "--label jiji.deployment=run-1",
            "--label jiji.lease=100.64.0.9",
            "--restart no",
            "--detach",
            "--env-file /var/lib/jiji/demo/env/twitch",
            "-v twitch-data:/data",
            "--memory 512m",
            "--ip 100.64.0.9",
            "--network jiji-demo",
        ] {
            assert!(
                joined.contains(expected),
                "missing '{expected}' in: {joined}"
            );
        }
        assert!(args.ends_with(&[
            "npm".to_string(),
            "run".to_string(),
            "sync:twitch".to_string()
        ]));
    }

    #[test]
    fn render_cron_run_rejects_an_invalid_dns_address() {
        let mut broken = spec();
        broken.dns_address = "not-an-ip".into();
        let error = render_cron_run(
            &broken,
            Engine::Docker,
            "run-1",
            "name",
            "100.64.0.9".parse().unwrap(),
        )
        .unwrap_err();
        assert!(error.contains("not-an-ip"));
    }

    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    fn write_fake_engine(dir: &std::path::Path, script: &str) -> std::path::PathBuf {
        let path = dir.join("fake-engine");
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    }

    /// Pre-claims a run and its address lease exactly as `api.rs`'s `CronRun` handler does before
    /// spawning `execute_claimed_run`.
    fn claim_and_lease(store: &AgentStore, run_id: &str, address: Ipv4Addr) -> CronRun {
        let outcome = store
            .claim_cron_run(
                "demo",
                "twitch",
                "sync-twitch",
                crate::cron::CronRunCause::Manual,
                None,
                run_id,
                100,
            )
            .unwrap();
        let crate::cron::CronClaimOutcome::Claimed(run) = outcome else {
            panic!("expected Claimed, got {outcome:?}");
        };
        store
            .claim_address_lease(
                run_id,
                &crate::leases::cron_replica_id("twitch", "sync-twitch"),
                address,
            )
            .unwrap();
        run
    }

    #[tokio::test]
    async fn execute_claimed_run_succeeds_and_releases_the_address() {
        let dir = tempdir().unwrap();
        let engine_path = write_fake_engine(
            dir.path(),
            r#"case "$1" in
  wait) echo 0; exit 0 ;;
  *) exit 0 ;;
esac"#,
        );
        let store = Arc::new(Mutex::new(
            AgentStore::open(&dir.path().join("agent.sqlite3")).unwrap(),
        ));
        let address: Ipv4Addr = "100.64.0.9".parse().unwrap();
        let run = claim_and_lease(&store.lock().unwrap(), "run-1", address);

        execute_claimed_run_with_binary(
            engine_path.to_str().unwrap(),
            Arc::clone(&store),
            Engine::Docker,
            spec(),
            run.clone(),
            address,
        )
        .await;

        let finished = store
            .lock()
            .unwrap()
            .cron_run(&run.run_id)
            .unwrap()
            .unwrap();
        assert_eq!(finished.state, CronRunState::Succeeded);
        assert_eq!(finished.exit_code, Some(0));
        assert_eq!(
            finished.container_name.as_deref(),
            Some(cron_container_name("demo", "twitch", "sync-twitch", &run.run_id).as_str())
        );
        let lease = store
            .lock()
            .unwrap()
            .address_lease("run-1")
            .unwrap()
            .unwrap();
        assert_eq!(lease.state, "quarantined");
    }

    #[tokio::test]
    async fn execute_claimed_run_reports_a_nonzero_exit_as_failed() {
        let dir = tempdir().unwrap();
        let engine_path = write_fake_engine(
            dir.path(),
            r#"case "$1" in
  wait) echo 7; exit 0 ;;
  *) exit 0 ;;
esac"#,
        );
        let store = Arc::new(Mutex::new(
            AgentStore::open(&dir.path().join("agent.sqlite3")).unwrap(),
        ));
        let address: Ipv4Addr = "100.64.0.9".parse().unwrap();
        let run = claim_and_lease(&store.lock().unwrap(), "run-1", address);

        execute_claimed_run_with_binary(
            engine_path.to_str().unwrap(),
            Arc::clone(&store),
            Engine::Docker,
            spec(),
            run.clone(),
            address,
        )
        .await;

        let finished = store
            .lock()
            .unwrap()
            .cron_run(&run.run_id)
            .unwrap()
            .unwrap();
        assert_eq!(finished.state, CronRunState::Failed);
        assert_eq!(finished.exit_code, Some(7));
    }

    #[tokio::test]
    async fn execute_claimed_run_reports_a_start_failure_as_failed_without_a_container() {
        let dir = tempdir().unwrap();
        let engine_path = write_fake_engine(
            dir.path(),
            r#"case "$1" in
  run) echo "boom" >&2; exit 1 ;;
  *) exit 0 ;;
esac"#,
        );
        let store = Arc::new(Mutex::new(
            AgentStore::open(&dir.path().join("agent.sqlite3")).unwrap(),
        ));
        let address: Ipv4Addr = "100.64.0.9".parse().unwrap();
        let run = claim_and_lease(&store.lock().unwrap(), "run-1", address);

        execute_claimed_run_with_binary(
            engine_path.to_str().unwrap(),
            Arc::clone(&store),
            Engine::Docker,
            spec(),
            run.clone(),
            address,
        )
        .await;

        let finished = store
            .lock()
            .unwrap()
            .cron_run(&run.run_id)
            .unwrap()
            .unwrap();
        assert_eq!(finished.state, CronRunState::Failed);
        assert!(finished.error.unwrap().contains("boom"));
        assert!(
            finished.started_at.is_none(),
            "a start failure must never record a started_at"
        );
    }

    #[tokio::test]
    async fn execute_claimed_run_stops_and_reports_timed_out_when_it_overruns() {
        let dir = tempdir().unwrap();
        let marker = dir.path().join("waited-once");
        let engine_path = write_fake_engine(
            dir.path(),
            &format!(
                r#"case "$1" in
  wait)
    if [ ! -f "{marker}" ]; then
      touch "{marker}"
      sleep 3
    fi
    echo 0
    exit 0
    ;;
  *) exit 0 ;;
esac"#,
                marker = marker.display()
            ),
        );
        let store = Arc::new(Mutex::new(
            AgentStore::open(&dir.path().join("agent.sqlite3")).unwrap(),
        ));
        let address: Ipv4Addr = "100.64.0.9".parse().unwrap();
        let run = claim_and_lease(&store.lock().unwrap(), "run-1", address);
        let mut short_timeout_spec = spec();
        short_timeout_spec.timeout_seconds = 1;

        execute_claimed_run_with_binary(
            engine_path.to_str().unwrap(),
            Arc::clone(&store),
            Engine::Docker,
            short_timeout_spec,
            run.clone(),
            address,
        )
        .await;

        let finished = store
            .lock()
            .unwrap()
            .cron_run(&run.run_id)
            .unwrap()
            .unwrap();
        assert_eq!(finished.state, CronRunState::TimedOut);
        assert!(finished
            .error
            .unwrap()
            .contains("exceeded its configured timeout"));
    }

    #[tokio::test]
    async fn recovery_finalizes_a_matching_exited_container_from_its_real_exit_code() {
        let dir = tempdir().unwrap();
        let engine_path = write_fake_engine(
            dir.path(),
            r#"case "$1" in
  ps) echo "demo-twitch-cron-sync-twitch-run1|run-1|exited" ;;
  inspect) echo 7 ;;
  *) exit 0 ;;
esac"#,
        );
        let store = Arc::new(Mutex::new(
            AgentStore::open(&dir.path().join("agent.sqlite3")).unwrap(),
        ));
        let address: Ipv4Addr = "100.64.0.9".parse().unwrap();
        let run = claim_and_lease(&store.lock().unwrap(), "run-1", address);

        recover_claimed_runs_with_binary(
            engine_path.to_str().unwrap(),
            Arc::clone(&store),
            Engine::Docker,
            "demo",
        )
        .await;

        let finished = store
            .lock()
            .unwrap()
            .cron_run(&run.run_id)
            .unwrap()
            .unwrap();
        assert_eq!(finished.state, CronRunState::Failed);
        assert_eq!(finished.exit_code, Some(7));
    }

    #[tokio::test]
    async fn recovery_fails_an_active_run_with_no_matching_container_and_releases_its_address() {
        let dir = tempdir().unwrap();
        let engine_path = write_fake_engine(dir.path(), "case \"$1\" in *) exit 0 ;; esac");
        let store = Arc::new(Mutex::new(
            AgentStore::open(&dir.path().join("agent.sqlite3")).unwrap(),
        ));
        let address: Ipv4Addr = "100.64.0.9".parse().unwrap();
        let run = claim_and_lease(&store.lock().unwrap(), "run-1", address);

        recover_claimed_runs_with_binary(
            engine_path.to_str().unwrap(),
            Arc::clone(&store),
            Engine::Docker,
            "demo",
        )
        .await;

        let finished = store
            .lock()
            .unwrap()
            .cron_run(&run.run_id)
            .unwrap()
            .unwrap();
        assert_eq!(finished.state, CronRunState::Failed);
        assert!(finished.error.unwrap().contains("missing"));
        let lease = store
            .lock()
            .unwrap()
            .address_lease("run-1")
            .unwrap()
            .unwrap();
        assert_eq!(lease.state, "quarantined");
    }

    #[tokio::test]
    async fn recovery_removes_an_unclaimed_managed_cron_container() {
        let dir = tempdir().unwrap();
        let stop_marker = dir.path().join("stopped");
        let rm_marker = dir.path().join("removed");
        let engine_path = write_fake_engine(
            dir.path(),
            &format!(
                r#"case "$1" in
  ps) echo "orphan-container|unknown-run-999|running" ;;
  stop) touch "{stop}" ;;
  rm) touch "{rm}" ;;
  *) exit 0 ;;
esac"#,
                stop = stop_marker.display(),
                rm = rm_marker.display(),
            ),
        );
        let store = Arc::new(Mutex::new(
            AgentStore::open(&dir.path().join("agent.sqlite3")).unwrap(),
        ));

        recover_claimed_runs_with_binary(
            engine_path.to_str().unwrap(),
            Arc::clone(&store),
            Engine::Docker,
            "demo",
        )
        .await;

        assert!(stop_marker.exists());
        assert!(rm_marker.exists());
    }

    #[tokio::test]
    async fn recovery_resumes_observation_of_a_still_running_container() {
        let dir = tempdir().unwrap();
        let engine_path = write_fake_engine(
            dir.path(),
            r#"case "$1" in
  ps) echo "demo-twitch-cron-sync-twitch-run1|run-1|running" ;;
  wait) echo 0; exit 0 ;;
  *) exit 0 ;;
esac"#,
        );
        let store = Arc::new(Mutex::new(
            AgentStore::open(&dir.path().join("agent.sqlite3")).unwrap(),
        ));
        let address: Ipv4Addr = "100.64.0.9".parse().unwrap();
        let run = claim_and_lease(&store.lock().unwrap(), "run-1", address);
        store
            .lock()
            .unwrap()
            .start_cron_run(
                &run.run_id,
                now_secs(),
                &run.run_id,
                "demo-twitch-cron-sync-twitch-run1",
                &address.to_string(),
            )
            .unwrap();
        store.lock().unwrap().apply_cron_spec(&spec()).unwrap();

        recover_claimed_runs_with_binary(
            engine_path.to_str().unwrap(),
            Arc::clone(&store),
            Engine::Docker,
            "demo",
        )
        .await;

        // Resumption is a spawned background task (recovery itself must not block startup on it);
        // poll briefly rather than sleeping a fixed, possibly-flaky duration.
        for _ in 0..100 {
            if store
                .lock()
                .unwrap()
                .cron_run(&run.run_id)
                .unwrap()
                .unwrap()
                .state
                != CronRunState::Running
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let finished = store
            .lock()
            .unwrap()
            .cron_run(&run.run_id)
            .unwrap()
            .unwrap();
        assert_eq!(finished.state, CronRunState::Succeeded);
        assert_eq!(finished.exit_code, Some(0));
    }
}
