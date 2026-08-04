//! Autonomous repair of project-scoped host runtime state.
//!
//! Durable catalog records decide ownership; local observations decide what needs repair. Missing
//! or unreachable resources never produce tombstones. Every action is idempotent and retries with
//! bounded exponential backoff.

use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;
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
    let mut pending_route_targets = BTreeMap::new();
    loop {
        let outcomes = reconcile_once(
            &store,
            engine,
            &config,
            &startup_candidates,
            &mut pending_route_targets,
        )
        .await;
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
    pending_route_targets: &mut BTreeMap<String, Vec<Ipv4Addr>>,
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
    outcomes.push(RepairOutcome {
        component: "containers",
        result: reconcile_containers(store, engine, config, startup_candidates).await,
    });
    outcomes.push(RepairOutcome {
        component: "deployment_recovery",
        result: recover_startup_candidates(store, engine, config, startup_candidates).await,
    });
    outcomes.push(RepairOutcome {
        component: "proxy_routes",
        result: reconcile_proxy_routes(
            store,
            engine,
            config,
            startup_candidates,
            pending_route_targets,
        )
        .await,
    });
    outcomes
}

async fn reconcile_proxy_routes(
    store: &Arc<Mutex<AgentStore>>,
    engine: Engine,
    config: &MeshConfig,
    startup_candidates: &BTreeSet<String>,
    pending_route_targets: &mut BTreeMap<String, Vec<Ipv4Addr>>,
) -> Result<(), String> {
    if config.local_runtime.proxy_routes.is_empty() {
        return Ok(());
    }
    let catalog = store
        .lock()
        .map_err(|_| "local store lock poisoned".to_string())?
        .latest_catalog()
        .map_err(|error| error.to_string())?;
    if catalog.iter().any(|record| {
        record.state == DeploymentState::Candidate
            && !startup_candidates.contains(&record.deployment_id)
    }) {
        // A live CLI transaction owns this candidate. Re-applying the old catalog-derived route
        // between its proxy gate and Active commit would undo the cutover. Startup candidates are
        // different: the CLI vanished with the reboot and `deployment_recovery` runs first.
        return Ok(());
    }
    let listed = proxy_exec(engine, &["kamal-proxy", "list"], Duration::from_secs(10)).await?;
    for route in &config.local_runtime.proxy_routes {
        let mut desired = crate::catalog::active_healthy_winners(&catalog)
            .into_iter()
            .filter(|record| record.service == route.service)
            .map(|record| record.address)
            .collect::<Vec<_>>();
        desired.sort();
        desired.dedup();
        let current = current_route_targets(&listed, &route.route_name).unwrap_or_default();
        if !should_apply_route_change(&desired, &current, pending_route_targets, &route.route_name)
        {
            continue;
        }

        if desired.is_empty() {
            if listed.contains(&route.route_name) {
                proxy_exec(
                    engine,
                    &["kamal-proxy", "remove", &route.route_name],
                    Duration::from_secs(10),
                )
                .await?;
            }
            continue;
        }
        deploy_proxy_route(engine, route, desired).await?;
    }
    Ok(())
}

/// Decides whether `desired` should actually be applied to a route this tick, given kamal-proxy's
/// own currently-live `current` target set and the candidate (if any) pending from the previous
/// tick. Returns `false` (no action) both when nothing has changed and when a genuine-looking
/// mismatch hasn't yet been confirmed by a second, identical observation.
///
/// A route kamal-proxy already serves with exactly this target set (or is already absent, for a
/// scaled-to-zero service) needs no action -- this matters because this reconciliation runs on a
/// fixed timer independent of any live `jiji deploy`. Confirmed live: a background tick here can
/// read this host's own locally replicated catalog a moment before a sibling host's just-committed
/// cutover has finished propagating, computing a stale target set and clobbering a route a
/// concurrent deploy had just correctly set, reintroducing "no route to host" against an address
/// the current catalog view (wrongly) still called active. Comparing against kamal-proxy's own
/// live, already-applied state (not the local catalog snapshot) catches most of this, but the same
/// replication lag that produces a stale `current` can equally produce a stale `desired` -- also
/// confirmed live, a single tick computed a several-generations-stale address for a remote replica
/// even after `desired` stopped matching `current`. Requiring the identical candidate on two
/// consecutive ticks (roughly `reconcile_interval_secs` apart) rides out a one-tick replication
/// blip, since a genuinely converged value naturally repeats while a transient stale read rarely
/// recurs identically; a real drift (e.g. after a restart) still gets corrected, just one tick
/// later.
fn should_apply_route_change(
    desired: &[Ipv4Addr],
    current: &[Ipv4Addr],
    pending: &mut BTreeMap<String, Vec<Ipv4Addr>>,
    route_name: &str,
) -> bool {
    if desired == current {
        pending.remove(route_name);
        return false;
    }
    if pending.get(route_name).map(Vec::as_slice) != Some(desired) {
        pending.insert(route_name.to_string(), desired.to_vec());
        return false;
    }
    pending.remove(route_name);
    true
}

/// Parses `kamal-proxy list`'s table output for one route's currently configured target
/// addresses, sorted. Strips ANSI color codes (`ghcr.io/acidtib/kamal-proxy:jiji`'s `list` always
/// colorizes output, even over a non-interactive exec) and returns `None` if the route isn't
/// listed at all, so an absent route is never confused with an empty-but-listed one.
fn current_route_targets(listed: &str, route_name: &str) -> Option<Vec<std::net::Ipv4Addr>> {
    let cleaned = strip_ansi_codes(listed);
    let line = cleaned
        .lines()
        .find(|line| line.split_whitespace().next() == Some(route_name))?;
    let targets_column = line.split_whitespace().nth(3)?;
    let mut addresses = targets_column
        .split(',')
        .filter_map(|target| target.rsplit_once(':').map(|(host, _port)| host))
        .filter_map(|host| host.parse::<std::net::Ipv4Addr>().ok())
        .collect::<Vec<_>>();
    addresses.sort();
    Some(addresses)
}

/// Strips ANSI SGR escape sequences (`\x1b[<params>m`) such as the color codes
/// `ghcr.io/acidtib/kamal-proxy:jiji`'s `list` command always emits.
fn strip_ansi_codes(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if next == 'm' {
                    break;
                }
            }
            continue;
        }
        output.push(ch);
    }
    output
}

async fn deploy_proxy_route(
    engine: Engine,
    route: &crate::runtime::ProxyRouteSpec,
    mut addresses: Vec<std::net::Ipv4Addr>,
) -> Result<(), String> {
    addresses.sort();
    addresses.dedup();
    let mut args = vec![
        "kamal-proxy".to_string(),
        "deploy".to_string(),
        route.route_name.clone(),
    ];
    args.extend(
        addresses
            .into_iter()
            .map(|address| format!("--target={address}:{}", route.port)),
    );
    args.extend(route.deploy_args.iter().cloned());
    let borrowed = args.iter().map(String::as_str).collect::<Vec<_>>();
    proxy_exec(
        engine,
        &borrowed,
        Duration::from_secs(route.deploy_timeout_secs.max(1)),
    )
    .await?;
    Ok(())
}

async fn recover_startup_candidates(
    store: &Arc<Mutex<AgentStore>>,
    engine: Engine,
    config: &MeshConfig,
    startup_candidates: &BTreeSet<String>,
) -> Result<(), String> {
    if startup_candidates.is_empty() {
        return Ok(());
    }
    let observations = match discovery::discover(engine, &config.project_id).await {
        DiscoveryOutcome::Observed(observations) => observations,
        DiscoveryOutcome::EngineUnavailable(error) | DiscoveryOutcome::EngineError(error) => {
            return Err(error)
        }
    };
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
    let missing_candidates = catalog
        .iter()
        .filter(|record| {
            startup_candidates.contains(&record.deployment_id)
                && record.owner_node_id == config.node_id
                && record.state == DeploymentState::Candidate
                && !running.contains_key(&record.deployment_id)
        })
        .map(|record| record.deployment_id.clone())
        .collect::<Vec<_>>();
    for candidate in catalog.iter().filter(|record| {
        startup_candidates.contains(&record.deployment_id)
            && record.owner_node_id == config.node_id
            && record.state == DeploymentState::Candidate
            && running.contains_key(&record.deployment_id)
    }) {
        let active_others = crate::catalog::active_healthy_winners(&catalog)
            .into_iter()
            .filter(|record| record.replica_id != candidate.replica_id)
            .map(|record| record.address)
            .collect::<Vec<_>>();
        for route in config
            .local_runtime
            .proxy_routes
            .iter()
            .filter(|route| route.service == candidate.service)
        {
            let mut addresses = active_others.clone();
            addresses.push(candidate.address);
            deploy_proxy_route(engine, route, addresses).await?;
        }

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
    if missing_candidates.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "startup candidate container(s) are absent and were preserved for operator recovery: {}",
            missing_candidates.join(", ")
        ))
    }
}

fn apply_local_catalog_state(
    store: &Arc<Mutex<AgentStore>>,
    config: &MeshConfig,
    original: &CatalogRecord,
    state: DeploymentState,
    health: crate::catalog::HealthState,
) -> Result<(), String> {
    let authority = config.keyring().map_err(|error| error.to_string())?;
    let signing_key = ed25519_dalek::SigningKey::from_bytes(
        config
            .node_signing_key
            .as_slice()
            .try_into()
            .map_err(|_| "local node signing key is invalid".to_string())?,
    );
    let store = store
        .lock()
        .map_err(|_| "local store lock poisoned".to_string())?;
    let membership = crate::membership::MembershipView::from_operations(
        store
            .membership_operations()
            .map_err(|error| error.to_string())?,
        &authority,
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
    let operation = crate::catalog::SignedCatalogOperation::sign(record, &signing_key)
        .map_err(|error| error.to_string())?;
    store
        .apply_catalog(
            &operation,
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
            "kamal-proxy",
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
            "kamal-proxy",
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
) -> Result<(), String> {
    let observations = match discovery::discover(engine, &config.project_id).await {
        DiscoveryOutcome::Observed(observations) => observations,
        DiscoveryOutcome::EngineUnavailable(error) | DiscoveryOutcome::EngineError(error) => {
            return Err(error)
        }
    };
    let catalog = store
        .lock()
        .map_err(|_| "local store lock poisoned".to_string())?
        .latest_catalog()
        .map_err(|error| error.to_string())?;
    for name in restart_candidates(&config.node_id, &observations, &catalog, startup_candidates) {
        command_required(engine.as_str(), &["start", &name]).await?;
    }
    Ok(())
}

pub fn restart_candidates(
    local_node_id: &str,
    observations: &[Observation],
    catalog: &[CatalogRecord],
    startup_candidates: &BTreeSet<String>,
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
                                && startup_candidates.contains(deployment)))
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
    command.arg("kamal-proxy").args(args);
    let output = tokio::time::timeout(timeout, command.output())
        .await
        .map_err(|_| format!("kamal-proxy command exceeded {}s", timeout.as_secs()))?
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

    #[test]
    fn current_route_targets_parses_a_real_ansi_colored_capture() {
        let listed = "\u{1b}[3;94mService\u{1b}[0m            \u{1b}[3;94mHost\u{1b}[0m              \u{1b}[3;94mPath\u{1b}[0m  \u{1b}[3;94mTarget\u{1b}[0m                            \u{1b}[3;94mState\u{1b}[0m    \u{1b}[3;94mTLS\u{1b}[0m  \n\u{1b}[1;34mphase9live-web-80\u{1b}[0m  \u{1b}[mphase9live.local\u{1b}[0m  \u{1b}[m/\u{1b}[0m     \u{1b}[m100.68.128.6:80,100.123.192.5:80\u{1b}[0m  \u{1b}[mrunning\u{1b}[0m  \u{1b}[mno\u{1b}[0m  \n";
        let targets: Vec<std::net::Ipv4Addr> =
            current_route_targets(listed, "phase9live-web-80").unwrap();
        assert_eq!(
            targets,
            vec![
                "100.68.128.6".parse::<std::net::Ipv4Addr>().unwrap(),
                "100.123.192.5".parse::<std::net::Ipv4Addr>().unwrap(),
            ]
        );
    }

    #[test]
    fn current_route_targets_is_none_for_an_unlisted_route() {
        let listed = "Service  Host  Path  Target  State  TLS\n";
        assert!(current_route_targets(listed, "phase9live-web-80").is_none());
    }

    #[test]
    fn matching_desired_and_current_never_touches_the_route() {
        let mut pending = BTreeMap::new();
        let addrs = vec!["10.0.0.5".parse().unwrap()];
        assert!(!should_apply_route_change(
            &addrs,
            &addrs,
            &mut pending,
            "r"
        ));
        assert!(pending.is_empty());
    }

    #[test]
    fn a_first_time_mismatch_is_held_back_pending_confirmation() {
        let mut pending = BTreeMap::new();
        let desired = vec!["10.0.0.6".parse().unwrap()];
        let current = vec!["10.0.0.5".parse().unwrap()];
        assert!(!should_apply_route_change(
            &desired,
            &current,
            &mut pending,
            "r"
        ));
        assert_eq!(pending.get("r"), Some(&desired));
    }

    #[test]
    fn the_same_mismatch_confirmed_on_a_second_tick_is_applied() {
        let mut pending = BTreeMap::new();
        let desired = vec!["10.0.0.6".parse().unwrap()];
        let current = vec!["10.0.0.5".parse().unwrap()];
        assert!(!should_apply_route_change(
            &desired,
            &current,
            &mut pending,
            "r"
        ));
        assert!(should_apply_route_change(
            &desired,
            &current,
            &mut pending,
            "r"
        ));
        assert!(pending.is_empty());
    }

    #[test]
    fn a_different_second_tick_value_resets_the_debounce_instead_of_confirming() {
        // Regression guard for the exact live failure: a stale replicated read (10.0.0.5) held back
        // for one tick, then superseded by a DIFFERENT stale read (10.0.0.6) on the next tick,
        // must never be treated as "confirmed" just because something changed twice in a row.
        let mut pending = BTreeMap::new();
        let current = vec!["10.0.0.4".parse().unwrap()];
        let first_guess = vec!["10.0.0.5".parse().unwrap()];
        let second_guess = vec!["10.0.0.6".parse().unwrap()];
        assert!(!should_apply_route_change(
            &first_guess,
            &current,
            &mut pending,
            "r"
        ));
        assert!(!should_apply_route_change(
            &second_guess,
            &current,
            &mut pending,
            "r"
        ));
        assert_eq!(pending.get("r"), Some(&second_guess));
    }

    #[test]
    fn current_route_targets_sorts_regardless_of_listed_order() {
        let listed = "phase9live-web-80  host  /  100.123.192.5:80,100.68.128.6:80  running  no\n";
        let targets: Vec<std::net::Ipv4Addr> =
            current_route_targets(listed, "phase9live-web-80").unwrap();
        assert_eq!(
            targets,
            vec![
                "100.68.128.6".parse::<std::net::Ipv4Addr>().unwrap(),
                "100.123.192.5".parse::<std::net::Ipv4Addr>().unwrap(),
            ]
        );
    }

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
            ),
            vec!["demo-web-deploy-1"]
        );
        assert!(restart_candidates(
            "node-b",
            &[observation("exited", "active")],
            &[catalog(DeploymentState::Active)],
            &BTreeSet::new(),
        )
        .is_empty());
        assert!(restart_candidates(
            "node-a",
            &[observation("exited", "candidate")],
            &[catalog(DeploymentState::Candidate)],
            &BTreeSet::new(),
        )
        .is_empty());
        assert_eq!(
            restart_candidates(
                "node-a",
                &[observation("exited", "candidate")],
                &[catalog(DeploymentState::Candidate)],
                &BTreeSet::from(["deploy-1".to_string()]),
            ),
            vec!["demo-web-deploy-1"]
        );
        assert!(restart_candidates(
            "node-a",
            &[observation("exited", "draining")],
            &[catalog(DeploymentState::Draining)],
            &BTreeSet::new(),
        )
        .is_empty());
        assert!(restart_candidates(
            "node-a",
            &[observation("running", "active")],
            &[catalog(DeploymentState::Active)],
            &BTreeSet::new(),
        )
        .is_empty());
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
}
