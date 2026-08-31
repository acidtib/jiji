//! Autonomous repair of project-scoped host runtime state.
//!
//! Durable catalog records decide ownership; local observations decide what needs repair. Missing
//! or unreachable resources never produce tombstones. Every action is idempotent and retries with
//! bounded exponential backoff.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tokio::process::Command;

use crate::catalog::{CatalogRecord, DeploymentState};
use crate::discovery::{self, DiscoveryOutcome};
use crate::engine::Engine;
use crate::runtime::MeshConfig;
use crate::store::{AgentStore, Observation};

const MIN_BACKOFF: Duration = Duration::from_secs(2);
const MAX_BACKOFF: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairOutcome {
    pub component: &'static str,
    pub result: Result<(), String>,
}

#[derive(Debug, Clone)]
struct Backoff {
    failures: u32,
}

impl Backoff {
    fn new() -> Self {
        Self { failures: 0 }
    }

    fn success(&mut self) -> Duration {
        self.failures = 0;
        MIN_BACKOFF
    }

    fn failure(&mut self) -> Duration {
        self.failures = self.failures.saturating_add(1);
        let multiplier = 1_u64 << self.failures.saturating_sub(1).min(5);
        MIN_BACKOFF
            .saturating_mul(multiplier as u32)
            .min(MAX_BACKOFF)
    }
}

pub async fn run_loop(
    store: Arc<Mutex<AgentStore>>,
    engine: Engine,
    config: MeshConfig,
    startup_candidates: BTreeSet<String>,
) {
    let mut backoff = Backoff::new();
    loop {
        let outcomes = reconcile_once(&store, engine, &config, &startup_candidates).await;
        let failed = outcomes.iter().any(|outcome| outcome.result.is_err());
        let delay = if failed {
            backoff.failure()
        } else {
            backoff.success()
        };
        let next_retry_at = unix_timestamp().saturating_add(delay.as_secs());
        if let Ok(store) = store.lock() {
            for outcome in &outcomes {
                let result = outcome.result.as_ref().map(|_| ()).map_err(String::as_str);
                if let Err(error) = store.record_component_result(
                    outcome.component,
                    result,
                    outcome.result.is_err().then_some(next_retry_at),
                ) {
                    tracing::warn!(%error, component = outcome.component, "repair diagnostic could not be recorded");
                }
            }
        }
        tokio::time::sleep(delay).await;
    }
}

/// Brings up just the two address-bearing links `runtime::run` binds sockets to
/// (`config.replication_bind`'s WireGuard management address, `config.dns_bind_address` on the
/// bridge) -- nothing else. Since Phase 9 removed the systemd-level `Requires=wg-quick@...`/
/// `jiji-network-restore-{slug}` ordering that used to guarantee these addresses existed before
/// the agent process even started, `main.rs` now calls this synchronously, with retries, before
/// spawning `runtime::run` and this module's own `run_loop` as independent concurrent tasks --
/// otherwise `runtime::run`'s `TcpListener::bind`/`dns::serve` can race the bridge/link coming up
/// and fail with `EADDRNOTAVAIL` (confirmed live: a real reboot hit this race and crash-looped the
/// whole agent, since neither address existed on any interface yet at the moment `runtime::run`
/// tried to bind).
pub async fn ensure_network_links(
    engine: Engine,
    config: &MeshConfig,
    store: &Arc<Mutex<AgentStore>>,
) -> Result<(), String> {
    ensure_link(
        &config.wireguard_interface,
        config.replication_bind.ip(),
        config.local_runtime.wireguard_port,
        &config.wireguard_private_key_path,
        store,
    )
    .await
    .map_err(|error| format!("WireGuard interface: {error}"))?;
    ensure_bridge_and_dns(engine, config)
        .await
        .map_err(|error| format!("bridge/DNS address: {error}"))?;
    Ok(())
}

pub async fn reconcile_once(
    store: &Arc<Mutex<AgentStore>>,
    engine: Engine,
    config: &MeshConfig,
    startup_candidates: &BTreeSet<String>,
) -> Vec<RepairOutcome> {
    let mut outcomes = Vec::new();
    outcomes.push(RepairOutcome {
        component: "wireguard",
        result: ensure_link(
            &config.wireguard_interface,
            config.replication_bind.ip(),
            config.local_runtime.wireguard_port,
            &config.wireguard_private_key_path,
            store,
        )
        .await,
    });
    let bridge_result = ensure_bridge_and_dns(engine, config).await;
    let bridge_rebuilt = matches!(bridge_result, Ok(true));
    outcomes.push(RepairOutcome {
        component: "bridge_dns",
        result: bridge_result.map(|_| ()),
    });
    if bridge_rebuilt {
        outcomes.push(RepairOutcome {
            component: "bridge_runtime_attachments",
            result: repair_runtime_attachments(engine, config).await,
        });
    }
    outcomes.push(RepairOutcome {
        component: "proxy_attachment",
        result: crate::proxy_bringup::reconcile(engine, config).await,
    });
    // Shared by containers/deployment_recovery/draining_cleanup below, avoiding three separate
    // `engine ps -a` spawns per tick.
    let discovery_result: Result<Vec<crate::store::Observation>, String> =
        match discovery::discover(engine, &config.project_id).await {
            DiscoveryOutcome::Observed(observations) => Ok(observations),
            DiscoveryOutcome::EngineUnavailable(error) | DiscoveryOutcome::EngineError(error) => {
                Err(error)
            }
        };
    outcomes.push(RepairOutcome {
        component: "containers",
        result: match &discovery_result {
            Ok(observations) => {
                reconcile_containers(store, engine, config, startup_candidates, observations).await
            }
            Err(error) => Err(error.clone()),
        },
    });
    outcomes.push(RepairOutcome {
        component: "image_retention",
        result: reconcile_image_retention(store, engine).await,
    });
    outcomes.push(RepairOutcome {
        component: "deployment_recovery",
        result: match &discovery_result {
            Ok(observations) => {
                recover_startup_candidates(store, engine, config, startup_candidates, observations)
                    .await
            }
            Err(error) => Err(error.clone()),
        },
    });
    outcomes.push(RepairOutcome {
        component: "candidate_health_gc",
        result: gc_candidate_health_checks(store, config).await,
    });
    outcomes.push(RepairOutcome {
        component: "draining_cleanup",
        result: match &discovery_result {
            Ok(observations) => {
                sweep_stuck_draining_records(store, engine, config, observations).await
            }
            Err(error) => Err(error.clone()),
        },
    });
    outcomes.push(RepairOutcome {
        component: "proxy_routes",
        result: reconcile_proxy_routes(engine, config).await,
    });
    outcomes.push(RepairOutcome {
        component: "tcp_proxy_routes",
        result: reconcile_tcp_routes(engine, config).await,
    });
    outcomes
}

/// Repairs routes that are absent from jiji-proxy. The setup-time config is a fallback, not the
/// authority for an existing route: deploy can change its backend port or policy after setup.
/// Reapplying the stale fallback on every tick would overwrite that newer deploy-time route.
async fn reconcile_proxy_routes(engine: Engine, config: &MeshConfig) -> Result<(), String> {
    for route in &config.local_runtime.proxy_routes {
        if !proxy_route_exists(engine, route).await? {
            deploy_proxy_route(engine, config, route).await?;
        }
    }
    Ok(())
}

async fn proxy_route_exists(
    engine: Engine,
    route: &crate::runtime::ProxyRouteSpec,
) -> Result<bool, String> {
    let mut args = vec![
        "route".to_string(),
        "status".to_string(),
        format!("--host={}", route.host),
    ];
    if let Some(prefix) = &route.path_prefix {
        args.push(format!("--path-prefix={prefix}"));
    }
    let borrowed = args.iter().map(String::as_str).collect::<Vec<_>>();
    let output = proxy_exec(engine, &borrowed, Duration::from_secs(30)).await?;
    parse_route_exists(&output)
}

async fn deploy_proxy_route(
    engine: Engine,
    config: &MeshConfig,
    route: &crate::runtime::ProxyRouteSpec,
) -> Result<(), String> {
    let mut args = vec![
        "route".to_string(),
        "apply".to_string(),
        format!("--host={}", route.host),
        format!("--dns-server={}:53", config.dns_bind_address),
        format!("--name={}", route.name),
        format!("--port={}", route.port),
    ];
    if let Some(prefix) = &route.path_prefix {
        args.push(format!("--path-prefix={prefix}"));
    }
    args.extend(route.apply_args.iter().cloned());
    let borrowed = args.iter().map(String::as_str).collect::<Vec<_>>();
    proxy_exec(engine, &borrowed, Duration::from_secs(30)).await?;
    Ok(())
}

/// Mirrors `reconcile_proxy_routes` for raw TCP routes. An existing route can contain newer
/// deploy-time config, so the setup-time fallback repairs only an absent route.
async fn reconcile_tcp_routes(engine: Engine, config: &MeshConfig) -> Result<(), String> {
    for route in &config.local_runtime.tcp_routes {
        if !tcp_route_exists(engine, route.listen_port).await? {
            deploy_tcp_route(engine, config, route).await?;
        }
    }
    Ok(())
}

async fn tcp_route_exists(engine: Engine, listen_port: u16) -> Result<bool, String> {
    let output = proxy_exec(
        engine,
        &[
            "tcp-route",
            "status",
            &format!("--listen-port={listen_port}"),
        ],
        Duration::from_secs(30),
    )
    .await?;
    parse_route_exists(&output)
}

fn parse_route_exists(output: &str) -> Result<bool, String> {
    let value: Value = serde_json::from_str(output)
        .map_err(|error| format!("could not parse jiji-proxy route status: {error}"))?;
    value
        .get("route_exists")
        .and_then(Value::as_bool)
        .ok_or_else(|| "jiji-proxy route status has no boolean 'route_exists' field".to_string())
}

async fn deploy_tcp_route(
    engine: Engine,
    config: &MeshConfig,
    route: &crate::runtime::TcpRouteSpec,
) -> Result<(), String> {
    let mut args = vec![
        "tcp-route".to_string(),
        "apply".to_string(),
        format!("--listen-port={}", route.listen_port),
        format!("--dns-server={}:53", config.dns_bind_address),
        format!("--name={}", route.name),
        format!("--port={}", route.port),
    ];
    args.extend(route.apply_args.iter().cloned());
    let borrowed = args.iter().map(String::as_str).collect::<Vec<_>>();
    proxy_exec(engine, &borrowed, Duration::from_secs(30)).await?;
    Ok(())
}

async fn recover_startup_candidates(
    store: &Arc<Mutex<AgentStore>>,
    engine: Engine,
    config: &MeshConfig,
    startup_candidates: &BTreeSet<String>,
    observations: &[Observation],
) -> Result<(), String> {
    if startup_candidates.is_empty() {
        return Ok(());
    }
    let running = observations
        .iter()
        .filter(|observation| observation.state == "running")
        .filter_map(|observation| {
            let labels: Value = serde_json::from_str(&observation.labels_json).ok()?;
            let deployment = labels.get("jiji.deployment")?.as_str()?.to_string();
            Some((deployment, observation.name.clone()))
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let present_deployments = observations
        .iter()
        .filter_map(|observation| {
            let labels: Value = serde_json::from_str(&observation.labels_json).ok()?;
            labels.get("jiji.deployment")?.as_str().map(str::to_string)
        })
        .collect::<BTreeSet<_>>();
    let catalog = store
        .lock()
        .map_err(|_| "local store lock poisoned".to_string())?
        .latest_catalog()
        .map_err(|error| error.to_string())?;
    let missing_candidates =
        missing_startup_candidates(&catalog, &config.node_id, startup_candidates, &running);
    let recoverable =
        runnable_startup_candidates(&catalog, &config.node_id, startup_candidates, &running);

    let mut unverified_candidates = Vec::new();
    for candidate in &recoverable {
        // No recorded plan (older jiji-cli, or a path with only the generic readiness check
        // anyway) falls back to trusting "observed running".
        let recorded_spec = store
            .lock()
            .map_err(|_| "local store lock poisoned".to_string())?
            .candidate_health_check(&candidate.deployment_id)
            .map_err(|error| error.to_string())?;
        let verified = match recorded_spec {
            Some(spec) => crate::candidate_health::verify_locally(&spec).await.is_ok(),
            None => true,
        };
        if !verified {
            // Fail-safe: leave the record Candidate and mark it known-failed so
            // restart_candidates won't resurrect it if stopped.
            store
                .lock()
                .map_err(|_| "local store lock poisoned".to_string())?
                .mark_candidate_health_check_failed(&candidate.deployment_id, unix_timestamp())
                .map_err(|error| error.to_string())?;
            unverified_candidates.push(candidate.deployment_id.clone());
            continue;
        }

        // No explicit proxy push needed here: `reconcile_proxy_routes` (run every tick,
        // immediately after this function) keeps jiji-proxy's static route definitions applied
        // regardless, and jiji-proxy's own continuous DNS re-resolution against this project's
        // `.jiji` zone picks up this candidate's address on its own within one refresh interval
        // once it's marked Active/Healthy below -- unlike kamal-proxy's route model, there is no
        // separate "push this specific address" step for recovery to do.
        apply_local_catalog_state(
            store,
            config,
            candidate,
            DeploymentState::Active,
            crate::catalog::HealthState::Healthy,
        )?;

        for previous in catalog.iter().filter(|record| {
            record.replica_id == candidate.replica_id
                && record.deployment_id != candidate.deployment_id
                && record.owner_node_id == config.node_id
                && record.state == DeploymentState::Active
        }) {
            apply_local_catalog_state(
                store,
                config,
                previous,
                DeploymentState::Draining,
                crate::catalog::HealthState::Unknown,
            )?;
            let name = dynamic_container_name(
                &config.project_id,
                &previous.service,
                &previous.deployment_id,
            );
            if present_deployments.contains(&previous.deployment_id) {
                let _ = command_required(engine.as_str(), &["stop", &name]).await;
                command_required(engine.as_str(), &["rm", "-f", &name]).await?;
            }
            apply_local_catalog_state(
                store,
                config,
                previous,
                DeploymentState::Tombstoned,
                crate::catalog::HealthState::Unknown,
            )?;
            store
                .lock()
                .map_err(|_| "local store lock poisoned".to_string())?
                .quarantine_address_lease(
                    &previous.deployment_id,
                    unix_timestamp().saturating_add(crate::leases::DEFAULT_QUARANTINE_SECONDS),
                )
                .map_err(|error| error.to_string())?;
        }
    }
    if missing_candidates.is_empty() && unverified_candidates.is_empty() {
        Ok(())
    } else {
        let mut parts = Vec::new();
        if !missing_candidates.is_empty() {
            parts.push(format!("absent: {}", missing_candidates.join(", ")));
        }
        if !unverified_candidates.is_empty() {
            parts.push(format!(
                "failed health verification: {}",
                unverified_candidates.join(", ")
            ));
        }
        Err(format!(
            "startup candidate container(s) preserved for operator recovery ({})",
            parts.join("; ")
        ))
    }
}

/// `Candidate` records this restart is responsible for recovering (see `main.rs`'s
/// `startup_candidates` computation) whose container was not found running at all -- never
/// destructive, just reported so an operator knows to look.
fn missing_startup_candidates(
    catalog: &[CatalogRecord],
    local_node_id: &str,
    startup_candidates: &BTreeSet<String>,
    running: &std::collections::BTreeMap<String, String>,
) -> Vec<String> {
    catalog
        .iter()
        .filter(|record| {
            startup_candidates.contains(&record.deployment_id)
                && record.owner_node_id == local_node_id
                && record.state == DeploymentState::Candidate
                && !running.contains_key(&record.deployment_id)
        })
        .map(|record| record.deployment_id.clone())
        .collect()
}

/// `Candidate` records this restart is responsible for recovering whose container is observed
/// running: candidates for promotion, pending the health-check replay in
/// `recover_startup_candidates` above.
fn runnable_startup_candidates(
    catalog: &[CatalogRecord],
    local_node_id: &str,
    startup_candidates: &BTreeSet<String>,
    running: &std::collections::BTreeMap<String, String>,
) -> Vec<CatalogRecord> {
    catalog
        .iter()
        .filter(|record| {
            startup_candidates.contains(&record.deployment_id)
                && record.owner_node_id == local_node_id
                && record.state == DeploymentState::Candidate
                && running.contains_key(&record.deployment_id)
        })
        .cloned()
        .collect()
}

/// Prunes recorded health-check plans (`candidate_health.rs`) that no longer correspond to a
/// `Candidate`-state record owned by this node.
async fn gc_candidate_health_checks(
    store: &Arc<Mutex<AgentStore>>,
    config: &MeshConfig,
) -> Result<(), String> {
    store
        .lock()
        .map_err(|_| "local store lock poisoned".to_string())?
        .gc_candidate_health_checks(&config.node_id)
        .map(|_removed| ())
        .map_err(|error| error.to_string())
}

/// Retries clearing every `Draining` record owned by this node whose removal was deferred because
/// a `network_mode: service:<this>` dependent was still attached (`is_dependent_container_error`
/// below). Mirrors `jiji-cli`'s `deploy_transaction::sweep_stuck_draining_records`, but runs every
/// tick instead of only after a deploy of the right service.
async fn sweep_stuck_draining_records(
    store: &Arc<Mutex<AgentStore>>,
    engine: Engine,
    config: &MeshConfig,
    observations: &[Observation],
) -> Result<(), String> {
    let catalog = store
        .lock()
        .map_err(|_| "local store lock poisoned".to_string())?
        .latest_catalog()
        .map_err(|error| error.to_string())?;
    let stuck = catalog
        .iter()
        .filter(|record| {
            record.owner_node_id == config.node_id && record.state == DeploymentState::Draining
        })
        .cloned()
        .collect::<Vec<_>>();
    if stuck.is_empty() {
        return Ok(());
    }
    let present_deployments = observations
        .iter()
        .filter_map(|observation| {
            let labels: Value = serde_json::from_str(&observation.labels_json).ok()?;
            labels.get("jiji.deployment")?.as_str().map(str::to_string)
        })
        .collect::<BTreeSet<_>>();
    let mut problems = Vec::new();
    for record in &stuck {
        let name =
            dynamic_container_name(&config.project_id, &record.service, &record.deployment_id);
        if present_deployments.contains(&record.deployment_id) {
            let _ = command_required(engine.as_str(), &["stop", &name]).await;
            if let Err(error) = command_required(engine.as_str(), &["rm", "-f", &name]).await {
                if !is_dependent_container_error(&error) {
                    problems.push(format!("{}: {error}", record.service));
                }
                continue;
            }
        }
        apply_local_catalog_state(
            store,
            config,
            record,
            DeploymentState::Tombstoned,
            crate::catalog::HealthState::Unknown,
        )?;
        store
            .lock()
            .map_err(|_| "local store lock poisoned".to_string())?
            .quarantine_address_lease(
                &record.deployment_id,
                unix_timestamp().saturating_add(crate::leases::DEFAULT_QUARANTINE_SECONDS),
            )
            .map_err(|error| error.to_string())?;
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

/// Whether `error` indicates removal failed only because a `network_mode: service:<this>`
/// dependent is still attached. Mirrors `jiji-cli`'s copy; not shared, since `jiji-agent` never
/// depends on `jiji-cli`.
fn is_dependent_container_error(error: &str) -> bool {
    error.to_lowercase().contains("dependent container")
}

fn apply_local_catalog_state(
    store: &Arc<Mutex<AgentStore>>,
    config: &MeshConfig,
    original: &CatalogRecord,
    state: DeploymentState,
    health: crate::catalog::HealthState,
) -> Result<(), String> {
    let scope = config.scope();
    let store = store
        .lock()
        .map_err(|_| "local store lock poisoned".to_string())?;
    let membership = crate::membership::MembershipView::from_records(
        store
            .membership_operations()
            .map_err(|error| error.to_string())?,
        &scope,
    )
    .map_err(|error| error.to_string())?;
    let next_revision = store
        .latest_catalog()
        .map_err(|error| error.to_string())?
        .iter()
        .filter(|record| record.replica_id == original.replica_id)
        .map(|record| record.revision)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    let mut record = original.clone();
    record.revision = next_revision;
    record.state = state;
    record.health = health;
    store
        .apply_catalog(
            record,
            crate::membership::RecordProvenance::Local,
            &config.project_id,
            config.recovery_epoch,
            &membership,
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn dynamic_container_name(project: &str, service: &str, deployment_id: &str) -> String {
    format!(
        "{project}-{service}-{}",
        deployment_id.get(..12).unwrap_or(deployment_id)
    )
}

async fn ensure_link(
    interface: &str,
    management_address: std::net::IpAddr,
    listen_port: u16,
    private_key_path: &std::path::Path,
    store: &Arc<Mutex<AgentStore>>,
) -> Result<(), String> {
    let std::net::IpAddr::V4(management_address) = management_address else {
        return Err("WireGuard management address must be IPv4".to_string());
    };
    // Checking only that the link *object* exists (rather than that it actually has the
    // management address bound) is not enough: confirmed live, an interrupted earlier bring-up
    // attempt can leave the link created (`ip link add` already ran) but still down and
    // addressless (`wg set`/address/up never completed), and every later tick would then
    // short-circuit here forever, believing it was already done.
    if link_has_management_address(interface, management_address).await {
        return Ok(());
    }
    crate::wireguard_bringup::bring_up_interface(
        interface,
        management_address,
        listen_port,
        private_key_path,
    )
    .await?;
    if !link_has_management_address(interface, management_address).await {
        return Err(format!(
            "{interface} is still missing its management address after bring-up"
        ));
    }
    // The interface needed (re)configuring, so its actual peer set is unknown or stale regardless
    // of whether the link object was freshly created or merely addressless -- clear the durable
    // peer cache so `runtime.rs`'s next reconcile treats every currently active membership record
    // as needing a fresh `wg set`, rather than trusting a cache that no longer reflects kernel
    // state. Confirmed live: without this, a reboot brought the interface back up with no peers at
    // all, and the cache-diffing incremental reconciler saw no membership *change* and never
    // reapplied them, since it was designed only for post-boot changes, not full bootstrap (that
    // used to be `wg-quick up`'s job, reading every `[Peer]` from the rendered config file, before
    // Phase 9 removed it).
    store
        .lock()
        .map_err(|_| "local store lock poisoned".to_string())?
        .replace_peer_cache(&std::collections::BTreeMap::new())
        .map_err(|error| error.to_string())?;
    Ok(())
}

async fn link_has_management_address(interface: &str, address: std::net::Ipv4Addr) -> bool {
    let Ok(output) = Command::new("ip")
        .args(["-4", "address", "show", "dev", interface])
        .output()
        .await
    else {
        return false;
    };
    output.status.success()
        && String::from_utf8_lossy(&output.stdout).contains(&format!("{address}/32"))
}

async fn ensure_bridge_and_dns(engine: Engine, config: &MeshConfig) -> Result<bool, String> {
    let dns = config.dns_bind_address.to_string();
    let ready = command_ok(
        "ip",
        &[
            "-4",
            "address",
            "show",
            "dev",
            &config.local_runtime.bridge_interface,
        ],
    )
    .await;
    if ready {
        let output = Command::new("ip")
            .args([
                "-4",
                "address",
                "show",
                "dev",
                &config.local_runtime.bridge_interface,
            ])
            .output()
            .await
            .map_err(|error| error.to_string())?;
        if String::from_utf8_lossy(&output.stdout).contains(&dns) {
            return Ok(false);
        }
    }
    crate::bridge_bringup::bring_up_bridge_and_dns(
        engine,
        &config.wireguard_interface,
        config.dns_bind_address,
        &config.local_runtime,
    )
    .await?;
    let output = Command::new("ip")
        .args([
            "-4",
            "address",
            "show",
            "dev",
            &config.local_runtime.bridge_interface,
        ])
        .output()
        .await
        .map_err(|error| error.to_string())?;
    if output.status.success() && String::from_utf8_lossy(&output.stdout).contains(&dns) {
        Ok(true)
    } else {
        Err(format!(
            "bridge {} or DNS address {} is still missing after restore",
            config.local_runtime.bridge_interface, dns
        ))
    }
}

async fn repair_runtime_attachments(engine: Engine, config: &MeshConfig) -> Result<(), String> {
    // A deleted kernel bridge leaves Podman/Docker's persisted network metadata intact. Merely
    // recreating the bridge therefore makes `inspect` report healthy attachments whose veth
    // devices no longer exist. Recreate this project's shared-proxy attachment and restart only
    // this project's managed containers so the engine materializes fresh veth pairs.
    let _ = Command::new(engine.as_str())
        .args([
            "network",
            "disconnect",
            "--force",
            &config.local_runtime.bridge_network,
            jiji_network::CONTAINER_NAME,
        ])
        .output()
        .await;
    command_required(
        engine.as_str(),
        &[
            "network",
            "connect",
            "--ip",
            &config.local_runtime.proxy_address.to_string(),
            &config.local_runtime.bridge_network,
            jiji_network::CONTAINER_NAME,
        ],
    )
    .await?;

    let observations = match discovery::discover(engine, &config.project_id).await {
        DiscoveryOutcome::Observed(observations) => observations,
        DiscoveryOutcome::EngineUnavailable(error) | DiscoveryOutcome::EngineError(error) => {
            return Err(error)
        }
    };
    for observation in observations {
        let labels: Value =
            serde_json::from_str(&observation.labels_json).map_err(|error| error.to_string())?;
        if labels.get("jiji.catalog-managed").and_then(Value::as_str) != Some("true") {
            continue;
        }
        let _ = Command::new(engine.as_str())
            .args(["stop", &observation.name])
            .output()
            .await;
        command_required(engine.as_str(), &["start", &observation.name]).await?;
    }
    Ok(())
}

async fn reconcile_containers(
    store: &Arc<Mutex<AgentStore>>,
    engine: Engine,
    config: &MeshConfig,
    startup_candidates: &BTreeSet<String>,
    observations: &[Observation],
) -> Result<(), String> {
    let catalog = store
        .lock()
        .map_err(|_| "local store lock poisoned".to_string())?
        .latest_catalog()
        .map_err(|error| error.to_string())?;
    let known_failed = store
        .lock()
        .map_err(|_| "local store lock poisoned".to_string())?
        .failed_verification_candidates()
        .map_err(|error| error.to_string())?;
    for name in restart_candidates(
        &config.node_id,
        observations,
        &catalog,
        startup_candidates,
        &known_failed,
    ) {
        command_required(engine.as_str(), &["start", &name]).await?;
    }
    Ok(())
}

type RetentionPruneResult = Result<Vec<(String, String)>, String>;

/// Prunes every installed image-retention spec's repo against this host's own local image cache
/// (see `image_retention.rs`). A per-repo failure (e.g. the engine briefly unavailable) is folded
/// into one component-level error string, same granularity every other `reconcile_once` component
/// already reports at, without stopping the other repos in the same tick.
async fn reconcile_image_retention(
    store: &Arc<Mutex<AgentStore>>,
    engine: Engine,
) -> Result<(), String> {
    let specs = store
        .lock()
        .map_err(|_| "local store lock poisoned".to_string())?
        .image_retention_specs()
        .map_err(|error| error.to_string())?;
    let mut results = Vec::with_capacity(specs.len());
    for spec in specs {
        let outcome =
            crate::image_retention::prune_repo(engine, &spec.repo, spec.retain as usize).await;
        results.push((spec.service, outcome));
    }
    fold_retention_problems(results)
}

/// Pure fold, kept separate from the async per-repo `prune_repo` calls above so it's testable
/// with fixed inputs and no real engine, mirroring `restart_candidates`'s split from
/// `reconcile_containers`. Reports both a whole-repo failure (the outer `Result`, e.g. the engine
/// briefly unavailable) and a per-image removal failure inside an otherwise-successful listing
/// (e.g. `rmi` refused because the tag is still referenced elsewhere): the latter used to be
/// dropped entirely -- an `Ok(..)` at the outer level short-circuited the per-image inspection,
/// so a stuck image silently stopped retention from ever making progress on that repo again, with
/// no log line and no effect on this component's reported health.
fn fold_retention_problems(results: Vec<(String, RetentionPruneResult)>) -> Result<(), String> {
    let mut problems = Vec::new();
    for (service, result) in results {
        match result {
            Err(error) => problems.push(format!("{service}: {error}")),
            Ok(failures) => {
                for (image_id, error) in failures {
                    problems.push(format!(
                        "{service}: could not remove image '{image_id}': {error}"
                    ));
                }
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

/// `known_failed` excludes a `Candidate` whose recorded health check already failed, so a manual
/// stop isn't silently undone. Never applies to an `Active` record.
pub fn restart_candidates(
    local_node_id: &str,
    observations: &[Observation],
    catalog: &[CatalogRecord],
    startup_candidates: &BTreeSet<String>,
    known_failed: &BTreeSet<String>,
) -> Vec<String> {
    observations
        .iter()
        .filter(|observation| observation.state != "running")
        .filter_map(|observation| {
            let labels: Value = serde_json::from_str(&observation.labels_json).ok()?;
            let label = |key: &str| labels.get(key).and_then(Value::as_str);
            if label("jiji.catalog-managed") != Some("true") {
                return None;
            }
            let deployment = label("jiji.deployment")?;
            let lifecycle = label("jiji.lifecycle")?;
            if !matches!(lifecycle, "candidate" | "active") {
                return None;
            }
            catalog
                .iter()
                .any(|record| {
                    record.owner_node_id == local_node_id
                        && record.deployment_id == deployment
                        && (record.state == DeploymentState::Active
                            || (record.state == DeploymentState::Candidate
                                && startup_candidates.contains(deployment)
                                && !known_failed.contains(deployment)))
                })
                .then(|| observation.name.clone())
        })
        .collect()
}

async fn command_ok(binary: &str, args: &[&str]) -> bool {
    Command::new(binary)
        .args(args)
        .output()
        .await
        .is_ok_and(|output| output.status.success())
}

async fn command_required(binary: &str, args: &[&str]) -> Result<(), String> {
    let output = Command::new(binary)
        .args(args)
        .output()
        .await
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

async fn proxy_exec(engine: Engine, args: &[&str], timeout: Duration) -> Result<String, String> {
    let mut command = Command::new(engine.as_str());
    command.arg("exec");
    if engine == Engine::Podman {
        command.arg("--no-session");
    }
    command
        .arg(jiji_network::CONTAINER_NAME)
        .arg("jiji-proxy")
        .args(args);
    let output = tokio::time::timeout(timeout, command.output())
        .await
        .map_err(|_| format!("jiji-proxy command exceeded {}s", timeout.as_secs()))?
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{HealthState, CATALOG_PROTOCOL_VERSION, CATALOG_SCHEMA_VERSION};

    fn observation(state: &str, lifecycle: &str) -> Observation {
        Observation {
            container_id: "container-1".into(),
            name: "demo-web-deploy-1".into(),
            image: "nginx".into(),
            state: state.into(),
            labels_json: serde_json::json!({
                "jiji.catalog-managed": "true",
                "jiji.deployment": "deploy-1",
                "jiji.lifecycle": lifecycle,
            })
            .to_string(),
        }
    }

    #[test]
    fn route_status_parser_distinguishes_present_and_missing_routes() {
        assert!(parse_route_exists(r#"{"route_exists":true,"backends":[]}"#).unwrap());
        assert!(!parse_route_exists(r#"{"route_exists":false,"backends":[]}"#).unwrap());
    }

    #[test]
    fn route_status_parser_rejects_an_invalid_response() {
        let error = parse_route_exists(r#"{"backends":[]}"#).expect_err("missing status");
        assert!(error.contains("route_exists"));
    }

    fn catalog(state: DeploymentState) -> CatalogRecord {
        CatalogRecord {
            project_id: "demo".into(),
            recovery_epoch: 1,
            protocol_version: CATALOG_PROTOCOL_VERSION,
            schema_version: CATALOG_SCHEMA_VERSION,
            service: "web".into(),
            replica_id: "replica-1".into(),
            owner_node_id: "node-a".into(),
            owner_epoch: 1,
            revision: 1,
            deployment_id: "deploy-1".into(),
            address: "10.0.0.4".parse().unwrap(),
            ports: vec![80],
            image: "nginx".into(),
            state,
            health: HealthState::Healthy,
        }
    }

    #[test]
    fn only_positive_owned_lifecycle_evidence_restarts_a_container() {
        assert_eq!(
            restart_candidates(
                "node-a",
                &[observation("exited", "active")],
                &[catalog(DeploymentState::Active)],
                &BTreeSet::new(),
                &BTreeSet::new(),
            ),
            vec!["demo-web-deploy-1"]
        );
        assert!(restart_candidates(
            "node-b",
            &[observation("exited", "active")],
            &[catalog(DeploymentState::Active)],
            &BTreeSet::new(),
            &BTreeSet::new(),
        )
        .is_empty());
        assert!(restart_candidates(
            "node-a",
            &[observation("exited", "candidate")],
            &[catalog(DeploymentState::Candidate)],
            &BTreeSet::new(),
            &BTreeSet::new(),
        )
        .is_empty());
        assert_eq!(
            restart_candidates(
                "node-a",
                &[observation("exited", "candidate")],
                &[catalog(DeploymentState::Candidate)],
                &BTreeSet::from(["deploy-1".to_string()]),
                &BTreeSet::new(),
            ),
            vec!["demo-web-deploy-1"]
        );
        assert!(restart_candidates(
            "node-a",
            &[observation("exited", "draining")],
            &[catalog(DeploymentState::Draining)],
            &BTreeSet::new(),
            &BTreeSet::new(),
        )
        .is_empty());
        assert!(restart_candidates(
            "node-a",
            &[observation("running", "active")],
            &[catalog(DeploymentState::Active)],
            &BTreeSet::new(),
            &BTreeSet::new(),
        )
        .is_empty());
    }

    #[test]
    fn known_failed_candidate_is_not_resurrected_once_stopped() {
        // Already known-failed -> not restarted.
        assert!(restart_candidates(
            "node-a",
            &[observation("exited", "candidate")],
            &[catalog(DeploymentState::Candidate)],
            &BTreeSet::from(["deploy-1".to_string()]),
            &BTreeSet::from(["deploy-1".to_string()]),
        )
        .is_empty());
        // known_failed never excludes an Active record.
        assert_eq!(
            restart_candidates(
                "node-a",
                &[observation("exited", "active")],
                &[catalog(DeploymentState::Active)],
                &BTreeSet::new(),
                &BTreeSet::from(["deploy-1".to_string()]),
            ),
            vec!["demo-web-deploy-1"]
        );
    }

    fn running_map(entries: &[&str]) -> std::collections::BTreeMap<String, String> {
        entries
            .iter()
            .map(|id| (id.to_string(), format!("container-{id}")))
            .collect()
    }

    #[test]
    fn runnable_startup_candidates_requires_ownership_state_and_running_evidence() {
        let candidate = catalog(DeploymentState::Candidate);
        let startup = BTreeSet::from(["deploy-1".to_string()]);

        assert_eq!(
            runnable_startup_candidates(
                std::slice::from_ref(&candidate),
                "node-a",
                &startup,
                &running_map(&["deploy-1"]),
            ),
            vec![candidate.clone()]
        );
        // Not in startup_candidates at all.
        assert!(runnable_startup_candidates(
            std::slice::from_ref(&candidate),
            "node-a",
            &BTreeSet::new(),
            &running_map(&["deploy-1"]),
        )
        .is_empty());
        // Owned by a different node.
        assert!(runnable_startup_candidates(
            std::slice::from_ref(&candidate),
            "node-b",
            &startup,
            &running_map(&["deploy-1"]),
        )
        .is_empty());
        // Already resolved past Candidate.
        assert!(runnable_startup_candidates(
            &[catalog(DeploymentState::Active)],
            "node-a",
            &startup,
            &running_map(&["deploy-1"]),
        )
        .is_empty());
        // Not observed running.
        assert!(
            runnable_startup_candidates(&[candidate], "node-a", &startup, &running_map(&[]))
                .is_empty()
        );
    }

    #[test]
    fn missing_startup_candidates_reports_only_absent_owned_candidates() {
        let candidate = catalog(DeploymentState::Candidate);
        let startup = BTreeSet::from(["deploy-1".to_string()]);

        assert_eq!(
            missing_startup_candidates(
                std::slice::from_ref(&candidate),
                "node-a",
                &startup,
                &running_map(&[])
            ),
            vec!["deploy-1".to_string()]
        );
        // Observed running -> not missing.
        assert!(missing_startup_candidates(
            std::slice::from_ref(&candidate),
            "node-a",
            &startup,
            &running_map(&["deploy-1"]),
        )
        .is_empty());
        // Owned by a different node -> not this node's problem to report.
        assert!(
            missing_startup_candidates(&[candidate], "node-b", &startup, &running_map(&[]))
                .is_empty()
        );
    }

    #[test]
    fn is_dependent_container_error_matches_only_that_failure_shape() {
        assert!(is_dependent_container_error(
            "Error: container 123 has dependent containers which must be removed before it"
        ));
        assert!(!is_dependent_container_error("no such container"));
        assert!(!is_dependent_container_error("permission denied"));
    }

    #[test]
    fn backoff_is_bounded_and_resets_after_success() {
        let mut backoff = Backoff::new();
        assert_eq!(backoff.failure(), Duration::from_secs(2));
        assert_eq!(backoff.failure(), Duration::from_secs(4));
        for _ in 0..20 {
            assert!(backoff.failure() <= MAX_BACKOFF);
        }
        assert_eq!(backoff.success(), MIN_BACKOFF);
        assert_eq!(backoff.failure(), Duration::from_secs(2));
    }

    #[test]
    fn fold_retention_problems_is_ok_when_every_repo_succeeds() {
        let results = vec![
            ("web".to_string(), Ok(vec![])),
            ("worker".to_string(), Ok(vec![])),
        ];
        assert_eq!(fold_retention_problems(results), Ok(()));
    }

    #[test]
    fn fold_retention_problems_reports_a_failing_repo_without_dropping_the_others() {
        let results = vec![
            ("web".to_string(), Ok(vec![])),
            ("worker".to_string(), Err("engine unavailable".to_string())),
        ];
        let error = fold_retention_problems(results).unwrap_err();
        assert!(error.contains("worker: engine unavailable"));
        assert!(!error.contains("web:"));
    }

    #[test]
    fn fold_retention_problems_joins_every_failing_repo() {
        let results = vec![
            ("web".to_string(), Err("timeout".to_string())),
            ("worker".to_string(), Err("engine unavailable".to_string())),
        ];
        let error = fold_retention_problems(results).unwrap_err();
        assert!(error.contains("web: timeout"));
        assert!(error.contains("worker: engine unavailable"));
    }

    #[test]
    fn fold_retention_problems_reports_a_failed_image_inside_an_otherwise_ok_listing() {
        let results = vec![(
            "web".to_string(),
            Ok(vec![(
                "img2".to_string(),
                "image is referenced in multiple repositories".to_string(),
            )]),
        )];
        let error = fold_retention_problems(results).unwrap_err();
        assert!(error.contains("web:"));
        assert!(error.contains("img2"));
        assert!(error.contains("image is referenced in multiple repositories"));
    }
}
