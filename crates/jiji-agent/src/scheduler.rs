//! The cron scheduler (`plans/service-cron.md`'s "Scheduler Rules" and Phase 4): ticks over every
//! installed `CronJobSpec`, claims a due run, and periodically retains old run metadata/containers.
//! Runs entirely inside `jiji-agent`, never host crontabs or systemd timers -- and only starts
//! after `cron_exec::recover_claimed_runs` completes (`main.rs`), so it never races a resumed run.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracing::warn;

use crate::cron::{CronClaimOutcome, CronRunCause};
use crate::cron_exec;
use crate::cron_schedule;
use crate::engine::Engine;
use crate::runtime::MeshConfig;
use crate::store::AgentStore;

/// The plan's "Durable Storage" default: keep completed run metadata for 30 days regardless of
/// how many accumulate, but never fewer than the latest 100 per job (see `METADATA_RETAIN_LATEST`
/// and `AgentStore::retain_cron_runs`).
const METADATA_RETAIN_SECS: u64 = 30 * 24 * 3600;
const METADATA_RETAIN_LATEST: u32 = 100;
/// The plan's "Durable Storage" default: keep a completed run's container for 24 hours so `cron
/// logs` can still read its output (`cron_exec::cleanup_old_cron_containers`).
const CONTAINER_RETAIN_SECS: u64 = 24 * 3600;

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Runs forever: a schedule-evaluation tick every `tick_interval`, and a metadata/container
/// retention tick every `cleanup_interval`. Never returns; intended to be spawned as its own task
/// (mirrors `discovery::run_loop`/`local_reconcile::run_loop`'s shape).
pub async fn run_loop(
    store: Arc<Mutex<AgentStore>>,
    engine: Engine,
    mesh_config: Arc<MeshConfig>,
    project: String,
    tick_interval: Duration,
    cleanup_interval: Duration,
) {
    let mut ticker = tokio::time::interval(tick_interval);
    let mut cleanup_ticker = tokio::time::interval(cleanup_interval);
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                tick(&store, engine, &mesh_config, &project, now_secs()).await;
            }
            _ = cleanup_ticker.tick() => {
                let now = now_secs();
                match store.lock() {
                    Ok(store) => {
                        if let Err(error) = store.retain_cron_runs(now, METADATA_RETAIN_SECS, METADATA_RETAIN_LATEST) {
                            warn!(%error, "failed to apply cron run metadata retention");
                        }
                    }
                    Err(_) => warn!("local store lock poisoned; skipping cron metadata retention"),
                }
                cron_exec::cleanup_old_cron_containers(Arc::clone(&store), engine, now, CONTAINER_RETAIN_SECS).await;
            }
        }
    }
}

/// One evaluation pass over every installed spec. Exposed (not just `run_loop`-private) so tests
/// can drive it directly against a controllable `now` instead of waiting on real timer ticks (the
/// plan's Test Plan explicitly calls for this).
pub async fn tick(
    store: &Arc<Mutex<AgentStore>>,
    engine: Engine,
    mesh_config: &MeshConfig,
    project: &str,
    now: u64,
) {
    let specs = match store.lock() {
        Ok(store) => store.cron_specs().unwrap_or_default(),
        Err(_) => {
            warn!("local store lock poisoned; skipping this scheduler tick");
            return;
        }
    };
    for spec in specs {
        let due_at = match store.lock() {
            Ok(store) => store
                .cron_scheduler_state(&spec.service, &spec.cron_name)
                .unwrap_or(None)
                .and_then(|state| state.next_due_at),
            Err(_) => {
                warn!("local store lock poisoned; skipping this scheduler tick");
                return;
            }
        };

        let Some(due_at) = due_at else {
            // Never evaluated before (a freshly installed job): initialize forward from now
            // without claiming anything -- there is no prior schedule to have missed yet.
            match cron_schedule::next_due_at(&spec.schedule, &spec.timezone, now) {
                Ok(next) => set_next_due(store, &spec.service, &spec.cron_name, now, next),
                Err(error) => warn!(
                    %error,
                    service = %spec.service,
                    cron_name = %spec.cron_name,
                    "could not compute this cron job's initial schedule"
                ),
            }
            continue;
        };
        if due_at > now {
            continue; // not due yet
        }

        let run_id = generate_scheduled_run_id(project, &spec.service, &spec.cron_name, due_at);
        let claim = match store.lock() {
            Ok(store) => store.claim_cron_run(
                project,
                &spec.service,
                &spec.cron_name,
                CronRunCause::Scheduled,
                Some(due_at),
                &run_id,
                now,
            ),
            Err(_) => {
                warn!("local store lock poisoned; skipping this scheduler tick");
                return;
            }
        };
        match claim {
            Ok(CronClaimOutcome::Claimed(run)) => {
                let locked = match store.lock() {
                    Ok(locked) => locked,
                    Err(_) => {
                        warn!("local store lock poisoned; skipping this scheduler tick");
                        return;
                    }
                };
                if let Err(error) = cron_exec::lease_and_spawn(
                    &locked,
                    Arc::clone(store),
                    engine,
                    mesh_config,
                    spec.clone(),
                    run,
                    now,
                ) {
                    warn!(%error, service = %spec.service, cron_name = %spec.cron_name, "scheduled cron run could not start");
                }
            }
            // Already handled by `claim_cron_run` itself: a duplicate claim is an idempotent
            // replay of the same tick (nothing new to do), and an overlap-forbidden claim already
            // bumped the durable skip counter this job's `status` reports.
            Ok(
                CronClaimOutcome::DuplicateScheduledClaim(_)
                | CronClaimOutcome::OverlapForbidden { .. },
            ) => {}
            Err(error) => {
                warn!(%error, service = %spec.service, cron_name = %spec.cron_name, "could not claim a due cron run");
            }
        }

        // `missed_runs: skip` (the only supported value): advance from the *natural* next
        // occurrence after this due time only if it is still in the future. If it has also
        // already passed -- this tick fell behind by more than one scheduled interval, whether
        // from a long agent outage or just a slow tick -- skip straight to the next occurrence
        // after `now` instead of claiming every missed tick one by one.
        let natural_next = cron_schedule::next_due_at(&spec.schedule, &spec.timezone, due_at);
        let next = match natural_next {
            Ok(next) if next > now => next,
            Ok(_missed) => match cron_schedule::next_due_at(&spec.schedule, &spec.timezone, now) {
                Ok(next) => {
                    warn!(
                        service = %spec.service,
                        cron_name = %spec.cron_name,
                        resumes_at = next,
                        "skipped one or more missed scheduled runs per missed_runs: skip"
                    );
                    next
                }
                Err(error) => {
                    warn!(%error, service = %spec.service, cron_name = %spec.cron_name, "could not compute this cron job's next schedule");
                    continue;
                }
            },
            Err(error) => {
                warn!(%error, service = %spec.service, cron_name = %spec.cron_name, "could not compute this cron job's next schedule");
                continue;
            }
        };
        set_next_due(store, &spec.service, &spec.cron_name, now, next);
    }
}

fn set_next_due(
    store: &Arc<Mutex<AgentStore>>,
    service: &str,
    cron_name: &str,
    now: u64,
    next: u64,
) {
    match store.lock() {
        Ok(store) => {
            if let Err(error) =
                store.set_cron_scheduler_state(service, cron_name, Some(now), Some(next))
            {
                warn!(%error, service, cron_name, "failed to persist this cron job's next scheduled time");
            }
        }
        Err(_) => warn!("local store lock poisoned; could not persist scheduler state"),
    }
}

/// Mirrors `api.rs`'s `generate_cron_run_id`, salted with `due_at` instead of a wall-clock nonce:
/// determinism isn't required (the `(service, cron_name, scheduled_at)` unique constraint is
/// `claim_cron_run`'s real dedup authority, not this id), but it keeps two ticks landing on the
/// exact same due time from needing a random source at all.
fn generate_scheduled_run_id(project: &str, service: &str, cron_name: &str, due_at: u64) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(format!("scheduled\0{project}\0{service}\0{cron_name}\0{due_at}").as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cron::{CronJobSpec, CronMissedRuns, CronOverlap, CronRunFilter};
    use tempfile::tempdir;

    fn spec(cron_name: &str, schedule: &str) -> CronJobSpec {
        CronJobSpec {
            project: "demo".into(),
            service: "twitch".into(),
            cron_name: cron_name.into(),
            revision: 1,
            canonical_hash: "hash".into(),
            owner_node_id: "node-a".into(),
            owner_epoch: 1,
            server: "app-1".into(),
            source_deployment_id: "dep-a".into(),
            source_replica_id: "replica-a".into(),
            image: "ghcr.io/example/twitch-sync:latest".into(),
            schedule: schedule.into(),
            timezone: "UTC".into(),
            timeout_seconds: 3600,
            overlap: CronOverlap::Forbid,
            missed_runs: CronMissedRuns::Skip,
            command: vec!["npm".into(), "run".into(), "sync:twitch".into()],
            env_file_path: "/var/lib/jiji/demo/env/twitch".into(),
            mount_args: vec![],
            resource_args: vec![],
            bridge_network: "jiji-demo".into(),
            dns_address: "100.64.0.5".into(),
        }
    }

    // Mirrors `runtime.rs`'s own private test fixture of the same shape; only the
    // address-allocation fields (`local_runtime.{container_cidr,bridge_gateway,proxy_address}`,
    // `dns_bind_address`) are actually exercised by these tests.
    fn mesh_config() -> MeshConfig {
        MeshConfig {
            project_id: "demo".into(),
            recovery_epoch: 1,
            node_id: "node-a".into(),
            wireguard_interface: "jijitest".into(),
            wireguard_private_key_path: "/etc/jiji/network/demo/private.key".into(),
            replication_bind: "127.0.0.1:17444".parse().unwrap(),
            dns_bind_address: "127.0.0.2".parse().unwrap(),
            local_runtime: crate::runtime::LocalRuntimeConfig {
                bridge_network: "jiji-demo".into(),
                bridge_interface: "jijibdemo".into(),
                proxy_address: "127.0.0.3".parse().unwrap(),
                proxy_routes: vec![],
                tcp_routes: vec![],
                container_subnet: "198.18.2.0/24".parse().unwrap(),
                bridge_gateway: "198.18.2.1".parse().unwrap(),
                container_cidr: "198.18.0.0/16".parse().unwrap(),
                wireguard_port: 51820,
                peer_public_ips: vec![],
                public_host: "203.0.113.10".into(),
            },
            reconcile_interval_secs: 10,
            store_soft_quota_bytes: crate::runtime::DEFAULT_STORE_SOFT_QUOTA_BYTES,
            compaction_interval_secs: crate::runtime::DEFAULT_COMPACTION_INTERVAL_SECS,
            dns_forwarders: vec!["1.1.1.1".parse().unwrap(), "8.8.8.8".parse().unwrap()],
        }
    }

    fn store() -> AgentStore {
        let dir = tempdir().unwrap();
        AgentStore::open(&dir.keep().join("agent.sqlite3")).unwrap()
    }

    #[tokio::test]
    async fn first_tick_initializes_the_schedule_without_claiming_anything() {
        let store = Arc::new(Mutex::new(store()));
        store
            .lock()
            .unwrap()
            .apply_cron_spec(&spec("sync-twitch", "*/5 * * * *"))
            .unwrap();

        tick(
            &store,
            Engine::Docker,
            &mesh_config(),
            "demo",
            1_704_067_200,
        )
        .await;

        assert!(store
            .lock()
            .unwrap()
            .cron_runs(&CronRunFilter::default())
            .unwrap()
            .is_empty());
        let state = store
            .lock()
            .unwrap()
            .cron_scheduler_state("twitch", "sync-twitch")
            .unwrap()
            .unwrap();
        assert_eq!(state.next_due_at, Some(1_704_067_200 + 5 * 60));
    }

    #[tokio::test]
    async fn a_due_job_is_claimed_and_the_schedule_advances() {
        let store = Arc::new(Mutex::new(store()));
        store
            .lock()
            .unwrap()
            .apply_cron_spec(&spec("sync-twitch", "*/5 * * * *"))
            .unwrap();
        // Seed a due time in the past so the very next tick claims it (mirrors a fresh install
        // followed by a real tick, without needing two calls to `tick`).
        store
            .lock()
            .unwrap()
            .set_cron_scheduler_state("twitch", "sync-twitch", None, Some(1_704_067_200))
            .unwrap();

        tick(
            &store,
            Engine::Docker,
            &mesh_config(),
            "demo",
            1_704_067_200,
        )
        .await;

        let runs = store
            .lock()
            .unwrap()
            .cron_runs(&CronRunFilter::default())
            .unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].cause, CronRunCause::Scheduled);
        assert_eq!(runs[0].scheduled_at, Some(1_704_067_200));

        let state = store
            .lock()
            .unwrap()
            .cron_scheduler_state("twitch", "sync-twitch")
            .unwrap()
            .unwrap();
        assert_eq!(state.next_due_at, Some(1_704_067_200 + 5 * 60));
    }

    #[tokio::test]
    async fn a_duplicate_tick_at_the_same_due_time_claims_nothing_new() {
        let store = Arc::new(Mutex::new(store()));
        store
            .lock()
            .unwrap()
            .apply_cron_spec(&spec("sync-twitch", "*/5 * * * *"))
            .unwrap();
        store
            .lock()
            .unwrap()
            .set_cron_scheduler_state("twitch", "sync-twitch", None, Some(1_704_067_200))
            .unwrap();

        tick(
            &store,
            Engine::Docker,
            &mesh_config(),
            "demo",
            1_704_067_200,
        )
        .await;
        // A second tick before the schedule advances past this due time again must not exist in
        // practice (the schedule always advances past `now` on claim) -- but re-run the exact
        // scenario by resetting next_due_at back, simulating a crash between claim and persist.
        store
            .lock()
            .unwrap()
            .set_cron_scheduler_state("twitch", "sync-twitch", None, Some(1_704_067_200))
            .unwrap();
        tick(
            &store,
            Engine::Docker,
            &mesh_config(),
            "demo",
            1_704_067_200,
        )
        .await;

        let runs = store
            .lock()
            .unwrap()
            .cron_runs(&CronRunFilter::default())
            .unwrap();
        assert_eq!(
            runs.len(),
            1,
            "the scheduled-time unique constraint must dedup the replay"
        );
    }

    #[tokio::test]
    async fn overlap_forbidden_still_advances_the_schedule_and_counts_the_skip() {
        let store = Arc::new(Mutex::new(store()));
        store
            .lock()
            .unwrap()
            .apply_cron_spec(&spec("sync-twitch", "*/5 * * * *"))
            .unwrap();
        store
            .lock()
            .unwrap()
            .claim_cron_run(
                "demo",
                "twitch",
                "sync-twitch",
                CronRunCause::Manual,
                None,
                "manual-run",
                1_704_067_100,
            )
            .unwrap();
        store
            .lock()
            .unwrap()
            .set_cron_scheduler_state("twitch", "sync-twitch", None, Some(1_704_067_200))
            .unwrap();

        tick(
            &store,
            Engine::Docker,
            &mesh_config(),
            "demo",
            1_704_067_200,
        )
        .await;

        let runs = store
            .lock()
            .unwrap()
            .cron_runs(&CronRunFilter::default())
            .unwrap();
        assert_eq!(
            runs.len(),
            1,
            "the forbidden claim must never have inserted a scheduled run"
        );
        let state = store
            .lock()
            .unwrap()
            .cron_scheduler_state("twitch", "sync-twitch")
            .unwrap()
            .unwrap();
        assert_eq!(state.next_due_at, Some(1_704_067_200 + 5 * 60));
        assert_eq!(state.skipped_overlap_count, 1);
    }

    #[tokio::test]
    async fn falling_behind_skips_straight_to_the_next_occurrence_after_now() {
        let store = Arc::new(Mutex::new(store()));
        store
            .lock()
            .unwrap()
            .apply_cron_spec(&spec("sync-twitch", "*/5 * * * *"))
            .unwrap();
        // Due at t=0, but the tick doesn't run until t=1000 (many missed 5-minute occurrences in
        // between) -- `missed_runs: skip` must land on the next occurrence after *now* (1000),
        // not the one immediately after the stale due time (300).
        store
            .lock()
            .unwrap()
            .set_cron_scheduler_state("twitch", "sync-twitch", None, Some(0))
            .unwrap();

        tick(&store, Engine::Docker, &mesh_config(), "demo", 1000).await;

        let state = store
            .lock()
            .unwrap()
            .cron_scheduler_state("twitch", "sync-twitch")
            .unwrap()
            .unwrap();
        // Next multiple of 300 strictly after 1000 is 1200, not 300.
        assert_eq!(state.next_due_at, Some(1200));
    }

    #[test]
    fn scheduled_run_ids_are_deterministic_and_distinct_per_due_time() {
        let a = generate_scheduled_run_id("demo", "twitch", "sync-twitch", 100);
        let b = generate_scheduled_run_id("demo", "twitch", "sync-twitch", 100);
        let c = generate_scheduled_run_id("demo", "twitch", "sync-twitch", 200);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
