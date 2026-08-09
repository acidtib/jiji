use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use jiji_config::{validate_config, Config, ContainerEngine, NamedServer};
use jiji_network::{Ipv4Cidr, NetworkPlan, NetworkPlanner, ServerPlan};
use jiji_ssh::{CommandResult, SshPool, SshSession};
use jiji_tui::Ui;
use sha2::{Digest, Sha256};

use super::bridge::{BridgeMigration, BridgeProvisioner};
use crate::lock::{LockRequest, LockScope};
use crate::ssh_adapter;

// Every path lives under a project-scoped subdirectory of the shared `/etc/jiji/network` parent
// (`network_dir`), so multiple independent projects can each have their own generation-swap tree
// on one host without colliding. `pub(crate)`: several of these are reused as-is by
// `crate::network_teardown`, the inverse of this module.
pub(crate) fn network_dir(slug: &str) -> String {
    format!("/etc/jiji/network/{slug}")
}

pub(crate) fn private_key_path(slug: &str) -> String {
    format!("{}/private.key", network_dir(slug))
}

pub(crate) fn public_key_path(slug: &str) -> String {
    format!("{}/public.key", network_dir(slug))
}

pub(crate) fn wireguard_config_path(wireguard_interface: &str) -> String {
    format!("/etc/wireguard/{wireguard_interface}.conf")
}

fn network_generations(slug: &str) -> String {
    format!("{}/generations", network_dir(slug))
}

pub(crate) fn network_current(slug: &str) -> String {
    format!("{}/current", network_dir(slug))
}

// Version 5 separates immutable mesh artifacts from service-runtime/DNS
// artifacts. It is an intentional clean break: older monolithic installations
// must be torn down and set up again rather than activated through this code.
const NETWORK_ARTIFACT_VERSION: u32 = 5;

struct ConnectedHost {
    name: String,
    session: SshSession,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct InstalledGeneration {
    network: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ActivationDomains {
    mesh: bool,
}

pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    hosts: Option<&str>,
) -> anyhow::Result<()> {
    Ui::section("Network Setup:");

    let start = std::env::current_dir()?;
    let config_path = config_file.map(Path::new);
    let (config, path) =
        crate::config_loading::load_config_for_ssh(environment, config_path, &start).await?;
    Ui::say(&format!("Configuration loaded from: {}", path.display()), 1);

    let validation = validate_config(&config);
    if !validation.valid {
        Ui::error(&format!(
            "Configuration validation failed with {} error(s):",
            validation.errors.len()
        ));
        for error in &validation.errors {
            Ui::say(&format!("{}: {}", error.path, error.message), 1);
        }
        anyhow::bail!("Configuration is invalid; fix the errors above and try again");
    }

    let plan = NetworkPlanner::new()
        .plan(&config)
        .context("Could not build the private network plan")?;
    if !plan.enabled {
        Ui::say("Private networking is disabled; no changes were made.", 1);
        return Ok(());
    }

    let target_names = selected_host_names(&plan, hosts)?;

    // A dedicated connection purely to hold the project-maintenance lock: `apply` (and the
    // `connect_all`/`apply_connected` it drives) manages its own independent connect/close cycle.
    let ssh = config.ssh.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "No `ssh:` section is configured. Add at least `ssh.user:` before running network setup."
        )
    })?;
    let mut named_targets: Vec<(String, NamedServer)> = config
        .servers
        .iter()
        .filter(|(name, _)| target_names.contains(*name))
        .map(|(name, server)| (name.clone(), server.clone()))
        .collect();
    named_targets.sort_by(|a, b| a.0.cmp(&b.0));
    let pool = SshPool::new(ssh.max_concurrent_starts as usize);
    let mut lock_connect_options = Vec::with_capacity(named_targets.len());
    for (name, server) in &named_targets {
        lock_connect_options.push(ssh_adapter::connect_options(name, server, ssh)?);
    }
    let operations: Vec<_> = lock_connect_options
        .into_iter()
        .map(|options| move || async move { SshSession::connect(&options).await })
        .collect();
    let connections = pool.execute_concurrent(operations).await;
    let mut lock_sessions: BTreeMap<String, Arc<SshSession>> = BTreeMap::new();
    let mut lock_failures = Vec::new();
    for ((name, server), connection) in named_targets.iter().zip(connections) {
        match connection {
            Ok(session) => {
                lock_sessions.insert(name.clone(), Arc::new(session));
            }
            Err(error) => lock_failures.push(format!("{name} ({}): {error}", server.host)),
        }
    }
    if !lock_failures.is_empty() {
        for session in lock_sessions.values() {
            session.close().await;
        }
        anyhow::bail!(
            "Could not connect to server(s): {}. Restore SSH access and retry.",
            lock_failures.join(", ")
        );
    }
    let lock_requests: Vec<LockRequest> = named_targets
        .iter()
        .map(|(name, _)| LockRequest::new(LockScope::ProjectMaintenance, name.clone()))
        .collect();

    let setup_result = crate::commands::lock::with_locks(
        &pool,
        &lock_sessions,
        &config.project,
        lock_requests,
        "jiji network setup".to_string(),
        crate::commands::lock::AutomaticLockOptions {
            timeout: 300,
            force: false,
        },
        || async { apply(&config, &plan, &target_names).await },
    )
    .await;
    for session in lock_sessions.values() {
        session.close().await;
    }
    setup_result
}

pub(crate) async fn reconcile_for_deploy(
    _config: &Config,
    _plan: &NetworkPlan,
) -> anyhow::Result<()> {
    // Membership, WireGuard repair, DNS, catalog, and desired placement are agent-owned.
    // A service deployment must never compile or fan out a cluster-wide network generation.
    Ok(())
}

pub(crate) async fn reconcile_for_server_setup(
    config: &Config,
    plan: &NetworkPlan,
    target_names: &BTreeSet<String>,
) -> anyhow::Result<()> {
    if target_names.len() == plan.servers.len() {
        return reconcile_cluster(config, plan, "jiji server setup", true).await;
    }
    let hosts = connect_enrollment_hosts(config, target_names).await?;
    let result = apply_connected(config, plan, target_names, &hosts, true).await;
    close_all(&hosts).await;
    result.context(
        "Could not bootstrap the selected host through a reachable seed; existing offline peers are not required",
    )
}

async fn connect_enrollment_hosts(
    config: &Config,
    target_names: &BTreeSet<String>,
) -> anyhow::Result<Vec<ConnectedHost>> {
    let ssh = config
        .ssh
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No `ssh:` section is configured"))?;
    let mut names = target_names.iter().cloned().collect::<Vec<_>>();
    let mut seeds = config
        .servers
        .keys()
        .filter(|name| !target_names.contains(*name))
        .cloned()
        .collect::<Vec<_>>();
    seeds.sort();
    names.extend(seeds);
    let mut hosts = Vec::new();
    let mut seed_found = false;
    for name in names {
        let server = &config.servers[&name];
        let options = ssh_adapter::connect_options(&name, server, ssh)?;
        match SshSession::connect(&options).await {
            Ok(session) => {
                let is_target = target_names.contains(&name);
                Ui::say(&format!("{name} ({}): connected", server.host), 1);
                hosts.push(ConnectedHost { name, session });
                if !is_target {
                    seed_found = true;
                    break;
                }
            }
            Err(error) if target_names.contains(&name) => {
                close_all(&hosts).await;
                return Err(error).with_context(|| {
                    format!("Could not connect to required enrollment target '{name}'")
                });
            }
            Err(error) => {
                Ui::warn(&format!(
                    "Seed candidate {name} ({}) is unavailable: {error}",
                    server.host
                ));
            }
        }
    }
    if config.servers.len() > target_names.len() && !seed_found {
        close_all(&hosts).await;
        anyhow::bail!("No existing seed server is reachable for enrollment");
    }
    Ok(hosts)
}

async fn reconcile_cluster(
    config: &Config,
    plan: &NetworkPlan,
    retry_command: &str,
    report_current: bool,
) -> anyhow::Result<()> {
    let target_names = plan.servers.keys().cloned().collect::<BTreeSet<_>>();
    let hosts = connect_all(config).await.with_context(|| {
        format!(
            "Could not reach every configured server for network reconciliation. Restore SSH access and retry `{retry_command}`."
        )
    })?;
    let mut stale_hosts = Vec::new();
    for host in &hosts {
        let mesh_current = if retry_command == "jiji deploy" {
            // Membership and WireGuard are agent-owned. Deploy must neither
            // gate on nor mutate the old compiled mesh generation.
            true
        } else {
            match crate::network_guard::generation_is_current(&host.session, plan).await {
                Ok(current) => current,
                Err(error) => {
                    close_all(&hosts).await;
                    return Err(error).with_context(|| {
                            format!(
                                "Could not inspect the installed mesh generation on '{}'. Restore SSH access and retry `jiji deploy`.",
                                host.name
                            )
                        });
                }
            }
        };
        if !mesh_current {
            stale_hosts.push(format!("{} (mesh)", host.name));
        }
    }

    if stale_hosts.is_empty() {
        for host in &hosts {
            remove_legacy_service_runtime(&host.session, plan).await?;
        }
        close_all(&hosts).await;
        if report_current {
            Ui::say(
                &format!(
                    "Mesh {} is already active on every configured server.",
                    &plan.mesh_generation[..12]
                ),
                1,
            );
        }
        return Ok(());
    }

    Ui::section("Network Reconciliation:");
    Ui::say(
        &format!(
            "Network state changed; reconciling mesh {} on all configured servers.",
            &plan.mesh_generation[..12],
        ),
        1,
    );
    print_network_requirements(config, plan);
    let result = apply_connected(
        config,
        plan,
        &target_names,
        &hosts,
        retry_command != "jiji deploy",
    )
    .await;
    close_all(&hosts).await;
    result.context(format!(
        "Network reconciliation failed for stale host(s): {}. Fix the reported network error and retry `{retry_command}`.",
        stale_hosts.join(", "),
    ))
}

async fn apply(
    config: &Config,
    plan: &NetworkPlan,
    target_names: &BTreeSet<String>,
) -> anyhow::Result<()> {
    Ui::say(
        &format!(
            "Applying mesh {} to: {}",
            &plan.mesh_generation[..12],
            target_names.iter().cloned().collect::<Vec<_>>().join(", ")
        ),
        1,
    );
    print_network_requirements(config, plan);
    let hosts = connect_all(config).await?;
    let result = apply_connected(config, plan, target_names, &hosts, true).await;
    close_all(&hosts).await;
    result
}

async fn connect_all(config: &Config) -> anyhow::Result<Vec<ConnectedHost>> {
    if config.servers.is_empty() {
        anyhow::bail!("No servers are configured. Add at least one server and retry.");
    }
    let ssh = config.ssh.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "No `ssh:` section is configured. Add at least `ssh.user:` before running network setup."
        )
    })?;

    let mut configured_servers: Vec<(String, NamedServer)> =
        config.servers.clone().into_iter().collect();
    configured_servers.sort_by(|left, right| left.0.cmp(&right.0));
    let mut connect_options = Vec::with_capacity(configured_servers.len());
    for (name, server) in &configured_servers {
        connect_options.push(ssh_adapter::connect_options(name, server, ssh)?);
    }

    let pool = SshPool::new(ssh.max_concurrent_starts as usize);
    let operations: Vec<_> = connect_options
        .into_iter()
        .map(|options| move || async move { SshSession::connect(&options).await })
        .collect();
    let connection_results = pool.execute_concurrent(operations).await;
    let mut hosts = Vec::with_capacity(configured_servers.len());
    let mut connection_failures = Vec::new();
    for ((name, server), result) in configured_servers.iter().zip(connection_results) {
        match result {
            Ok(session) => {
                Ui::say(&format!("{name} ({}): connected", server.host), 1);
                hosts.push(ConnectedHost {
                    name: name.clone(),
                    session,
                });
            }
            Err(error) => {
                connection_failures.push(format!("{name} ({}): {error}", server.host));
            }
        }
    }
    if !connection_failures.is_empty() {
        for failure in &connection_failures {
            Ui::error(failure);
        }
        close_all(&hosts).await;
        anyhow::bail!(
            "Could not reach every configured server. Restore SSH access and retry network setup."
        );
    }
    Ok(hosts)
}

fn print_network_requirements(config: &Config, plan: &NetworkPlan) {
    Ui::say(
        "All configured servers must be reachable during initial WireGuard key enrollment.",
        1,
    );
    Ui::say(
        &format!(
            "WireGuard UDP port {} must be allowed between the configured server public IPs.",
            jiji_network::wireguard_port(&plan.project)
        ),
        1,
    );
    let uses_default_cidr = config.network.as_ref().is_none_or(|network| {
        network.container_cidr.is_none() || network.management_cidr.is_none()
    });
    if uses_default_cidr {
        Ui::say(
            &format!(
                "Using inferred project ranges: management {}, containers {}.",
                plan.management_cidr, plan.container_cidr
            ),
            1,
        );
    }
}

async fn apply_connected(
    config: &Config,
    plan: &NetworkPlan,
    target_names: &BTreeSet<String>,
    hosts: &[ConnectedHost],
    allow_mesh_changes: bool,
) -> anyhow::Result<()> {
    let slug = jiji_network::systemd_unit_slug(&plan.project);
    let mut migrations = BTreeMap::new();
    let mut mesh_current = BTreeMap::new();
    for host in hosts
        .iter()
        .filter(|host| target_names.contains(&host.name))
    {
        mesh_current.insert(
            host.name.clone(),
            !allow_mesh_changes
                || crate::network_guard::generation_is_current(&host.session, plan).await?,
        );
    }

    Ui::section("Preflight:");
    for host in hosts
        .iter()
        .filter(|host| target_names.contains(&host.name))
    {
        require_root(&host.session).await?;
        reject_monolithic_generation(&host.session, &slug).await?;
        ensure_engine_available(&host.session, config.builder.engine).await?;
        let migration = inspect_conflicts(
            &host.session,
            &host.name,
            &plan.servers[&host.name],
            plan,
            config.builder.engine,
            &slug,
        )
        .await?;
        if mesh_current[&host.name] {
            write_generation_file(
                &host.session,
                &format!("{}/address-ranges", network_current(&slug)),
                "0644",
                &format!("{} {}\n", plan.management_cidr, plan.container_cidr),
            )
            .await?;
            Ui::say(&format!("{}: mesh address ranges unchanged", host.name), 1);
            continue;
        }
        if let Some(migration) = migration {
            migrations.insert(host.name.clone(), migration);
            Ui::say(
                &format!(
                    "{}: project bridge will migrate to {}",
                    host.name, plan.servers[&host.name].container_subnet
                ),
                1,
            );
        } else {
            Ui::say(&format!("{}: address ranges are available", host.name), 1);
        }
    }

    Ui::section("Preparing Hosts:");
    for host in hosts
        .iter()
        .filter(|host| target_names.contains(&host.name))
    {
        if mesh_current[&host.name] {
            Ui::say(
                &format!("{}: existing mesh prerequisites retained", host.name),
                1,
            );
            continue;
        }
        install_prerequisites(&host.session, &slug).await?;
        ensure_keypair(&host.session, &slug).await?;
        Ui::say(
            &format!("{}: prerequisites and host key ready", host.name),
            1,
        );
    }

    let public_key_path = public_key_path(&slug);
    let mut public_keys = BTreeMap::new();
    for host in hosts {
        let result = host
            .session
            .execute(&format!(
                "test -s {public_key_path} && cat {public_key_path}"
            ))
            .await?;
        if !result.success || result.stdout.trim().is_empty() {
            anyhow::bail!(
                "Server '{}' has no enrolled WireGuard public key. Run network setup without a host filter once all servers are reachable.",
                host.name
            );
        }
        public_keys.insert(host.name.clone(), result.stdout.trim().to_string());
    }
    // A targeted `-H` run only ever connects the target(s) plus at most one seed (see
    // `connect_enrollment_hosts`), so the loop above never has a key for any *other* project
    // member. `artifact_generation` hashes over this whole map, though, and a hash computed from
    // only 1-2 keys will almost never match a generation last installed with the full project's
    // keys -- forcing a real re-activation, which renders a *new* wireguard.conf from this same
    // incomplete map (`render_wireguard` silently skips any peer it has no key for). Confirmed
    // live (2026-07-30): this silently dropped every other already-configured server as a
    // WireGuard peer on the targeted host, breaking cross-host replication until a full
    // `jiji network setup` was run to rebuild the complete peer set. Recovering an absent
    // member's key from whichever connected host most recently installed a full generation (they
    // must have connected to that member directly at the time) closes the gap without requiring
    // this invocation to reach every host itself.
    if public_keys.len() < plan.servers.len() {
        for host in hosts {
            if public_keys.len() == plan.servers.len() {
                break;
            }
            for (name, key) in read_installed_peer_keys(&host.session, &slug).await {
                if plan.servers.contains_key(&name) {
                    public_keys.entry(name).or_insert(key);
                }
            }
        }
    }
    let artifact_generation = artifact_generation(&plan.mesh_generation, &public_keys);
    let target_hosts = hosts
        .iter()
        .filter(|host| target_names.contains(&host.name))
        .collect::<Vec<_>>();
    let mut previous = BTreeMap::new();
    for host in &target_hosts {
        previous.insert(
            host.name.clone(),
            capture_installed_generation(&host.session, &slug).await?,
        );
    }
    let desired_network = format!("{}/{artifact_generation}", network_generations(&slug));
    let activation_domains = previous
        .iter()
        .map(|(name, installed)| {
            (
                name.clone(),
                ActivationDomains {
                    mesh: installed.network.as_deref() != Some(desired_network.as_str()),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();

    Ui::section("Staging Network Configuration:");
    for host in hosts
        .iter()
        .filter(|host| target_names.contains(&host.name))
    {
        let server_plan = &plan.servers[&host.name];
        stage_host(
            &host.session,
            config.builder.engine,
            plan,
            server_plan,
            &public_keys,
            &artifact_generation,
            &slug,
            activation_domains[&host.name],
        )
        .await
        .with_context(|| {
            format!(
                "Could not stage generation {} on '{}'. No host generation was activated. Fix the reported error and rerun `jiji network setup`.",
                plan.mesh_generation, host.name
            )
        })?;
        Ui::say(&format!("{}: generation staged", host.name), 1);
    }

    let rollback_context = RollbackContext {
        previous: &previous,
        activation_domains: &activation_domains,
        migrations: &migrations,
        plan,
        engine: config.builder.engine,
    };

    Ui::section("Activating Network:");
    let mut attempted = Vec::new();
    for host in &target_hosts {
        attempted.push(*host);
        let domains = activation_domains[&host.name];
        let bridge = BridgeProvisioner::new(config.builder.engine, plan, &plan.servers[&host.name]);
        if let Some(migration) = migrations.get(&host.name) {
            if let Err(error) = bridge.detach_for_migration(&host.session, migration).await {
                return rollback_transaction(
                    &attempted,
                    &rollback_context,
                    &host.name,
                    "bridge migration",
                    error,
                )
                .await;
            }
        }
        if let Err(error) = activate_host(
            &host.session,
            &plan.servers[&host.name],
            &plan.mesh_generation,
            &artifact_generation,
            &slug,
            domains.mesh,
        )
        .await
        {
            return rollback_transaction(
                &attempted,
                &rollback_context,
                &host.name,
                "activation",
                error,
            )
            .await;
        }
        if let Some(migration) = migrations.get(&host.name) {
            if let Err(error) = bridge
                .reattach_after_migration(&host.session, migration)
                .await
            {
                return rollback_transaction(
                    &attempted,
                    &rollback_context,
                    &host.name,
                    "container reattachment",
                    error,
                )
                .await;
            }
            if migration.includes_proxy() && config.builder.engine == ContainerEngine::Docker {
                let server_plan = &plan.servers[&host.name];
                let ingress_result = match crate::proxy::parse_public_host(server_plan) {
                    Ok(public_host) => {
                        crate::proxy_ingress::ensure_ingress_rule(
                            &host.session,
                            server_plan.proxy_address,
                            public_host,
                            true,
                            &[],
                        )
                        .await
                    }
                    Err(error) => Err(error),
                };
                if let Err(error) = ingress_result {
                    return rollback_transaction(
                        &attempted,
                        &rollback_context,
                        &host.name,
                        "proxy ingress migration",
                        error,
                    )
                    .await;
                }
            }
        }
        Ui::say(&format!("{}: generation activated", host.name), 1);
    }

    // Only the target host(s)' own generation is staged/activated above -- a seed connected purely
    // for enrollment (see `connect_enrollment_hosts`) never has its own interface touched by that
    // loop. Without this, the seed never dials out to a brand-new target first, which is fatal
    // (not just slow) for a cloud+home mixed topology: WireGuard can only ever learn a NATed peer's
    // real, currently-routable endpoint from that peer's own first packet (its built-in endpoint
    // roaming), and a target whose seed is a private-LAN-addressed home server can never reach that
    // declared address from the public internet at all until the seed reaches out first. Applying
    // this directly as a live `wg set` (matching exactly what `jiji-agent`'s own incremental
    // reconciliation -- `wireguard.rs::plan_reconciliation`/`runtime.rs::apply_action` -- would
    // eventually do on its own) closes that gap immediately rather than depending on signed
    // membership replication reaching the seed and its own reconcile tick running before
    // `verify_host` below's connectivity check, which was failing outright.
    if let Some(seed) = hosts.iter().find(|host| !target_names.contains(&host.name)) {
        for host in &target_hosts {
            if let Err(error) = enroll_target_with_seed(
                &seed.session,
                &plan.servers[&seed.name],
                &host.name,
                &public_keys,
            )
            .await
            {
                return rollback_transaction(
                    &attempted,
                    &rollback_context,
                    &host.name,
                    "seed enrollment",
                    error,
                )
                .await;
            }
        }
    }

    Ui::section("Verifying Network:");
    for host in &target_hosts {
        if let Err(error) = verify_host(
            &host.session,
            &plan.servers[&host.name],
            plan,
            activation_domains[&host.name],
            &public_keys,
        )
        .await
        {
            return rollback_transaction(
                &attempted,
                &rollback_context,
                &host.name,
                "verification",
                error,
            )
            .await;
        }
        let verified = if activation_domains[&host.name].mesh {
            "mesh"
        } else {
            "installed state"
        };
        Ui::say(&format!("{}: {verified} ready", host.name), 1);
    }

    for host in &target_hosts {
        remove_legacy_service_runtime(&host.session, plan).await?;
    }

    Ui::success("Private network setup completed.");
    Ok(())
}

/// Applies `target_name` as a live WireGuard peer directly on `seed`'s own interface. See the
/// call site in `apply_connected` for why this is necessary, not just an optimization.
async fn enroll_target_with_seed(
    seed_session: &SshSession,
    seed_server: &ServerPlan,
    target_name: &str,
    public_keys: &BTreeMap<String, String>,
) -> anyhow::Result<()> {
    let Some(peer) = seed_server
        .peers
        .iter()
        .find(|peer| peer.server == target_name)
    else {
        anyhow::bail!(
            "'{target_name}' is not a peer in the seed's own network plan; this should never happen for two servers in the same project"
        );
    };
    let public_key = public_keys.get(target_name).ok_or_else(|| {
        anyhow::anyhow!("no enrolled WireGuard public key was read for '{target_name}'")
    })?;
    let allowed_ips = peer
        .allowed_ips
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let command = format!(
        "wg set {} peer {public_key} endpoint {} allowed-ips {allowed_ips} persistent-keepalive 25",
        seed_server.wireguard_interface, peer.endpoint
    );
    let result = seed_session.execute(&command).await?;
    ensure_success(seed_session, &command, &result)
}

async fn remove_legacy_service_runtime(
    session: &SshSession,
    plan: &NetworkPlan,
) -> anyhow::Result<()> {
    let slug = jiji_network::systemd_unit_slug(&plan.project);
    let table = jiji_network::service_nat_table_name(&plan.project);
    let root = network_dir(&slug);
    // Each legacy unit is disabled in its own invocation (not one call listing both) since
    // `systemctl disable --now A B` aborts on the first unit name that doesn't exist without
    // touching the rest -- confirmed live to silently skip real units this way (see
    // `network_teardown.rs::stop_and_disable_units`'s doc comment for the reproduction).
    let command = format!(
        "set -eu; \
         systemctl disable --now jiji-dns-{slug}.service 2>/dev/null || true; \
         systemctl disable --now jiji-service-nat-{slug}.service 2>/dev/null || true; \
         rm -f /etc/systemd/system/jiji-dns-{slug}.service /etc/systemd/system/jiji-service-nat-{slug}.service \
           {root}/restore-service-nat.sh {root}/service-runtime-generation; \
         rm -rf {root}/dns-current {root}/dns-generations {root}/service-nat-current {root}/service-nat-generations; \
         nft delete table ip {table} 2>/dev/null || true; \
         systemctl daemon-reload"
    );
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)
}

async fn reject_monolithic_generation(session: &SshSession, slug: &str) -> anyhow::Result<()> {
    let legacy = format!("{}/generation", network_dir(slug));
    let result = session
        .execute(&format!("cat {legacy} 2>/dev/null || true"))
        .await?;
    if !result.stdout.trim().is_empty() {
        anyhow::bail!(
            "Host {} has a monolithic network generation installed. This development build requires clean separated mesh/service-runtime state; run `jiji server teardown` followed by `jiji server setup`.",
            session.host()
        );
    }
    Ok(())
}

fn selected_host_names(
    plan: &NetworkPlan,
    host_filters: Option<&str>,
) -> anyhow::Result<BTreeSet<String>> {
    let filters: Vec<String> = host_filters
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Ok(plan
        .select_hosts(&filters)?
        .into_iter()
        .map(|server| server.name.clone())
        .collect())
}

async fn require_root(session: &SshSession) -> anyhow::Result<()> {
    let result = session.execute("id -u").await?;
    if !result.success || result.stdout.trim() != "0" {
        anyhow::bail!(
            "Network setup on {} requires an SSH user with uid 0. Connect as root and retry.",
            session.host()
        );
    }
    Ok(())
}

/// Any bridge network jiji itself could have created, from any project (`naming::bridge_network_name`
/// always produces this prefix). Used to distinguish "another jiji project's bridge, expected to
/// coexist" from "foreign, non-jiji infrastructure, a real conflict" during preflight.
fn is_jiji_bridge_name(name: &str) -> bool {
    name.starts_with("jiji-")
}

/// Any WireGuard/bridge kernel interface jiji itself could have created, from any project
/// (`naming::wireguard_interface_name`/`bridge_interface_name` both produce this prefix).
fn is_jiji_managed_interface(name: &str) -> bool {
    name.starts_with("jiji")
}

async fn inspect_conflicts(
    session: &SshSession,
    server_name: &str,
    server_plan: &ServerPlan,
    plan: &NetworkPlan,
    engine: ContainerEngine,
    slug: &str,
) -> anyhow::Result<Option<BridgeMigration>> {
    let network_command = BridgeProvisioner::network_inspection_command(engine);
    let command = format!(
        "ip -o -4 route show table all; {network_command}; \
         wg show all listen-port 2>/dev/null | sed 's/^/PORT /' || true; \
         ip -o -4 address show | sed 's/^/ADDR /'; \
         for range_file in /etc/jiji/network/*/current/address-ranges; do \
           test -f \"$range_file\" || continue; \
           owner=${{range_file#/etc/jiji/network/}}; owner=${{owner%%/*}}; \
           test \"$owner\" = \"{slug}\" && continue; \
           read management container < \"$range_file\" || continue; \
           printf 'RANGE %s %s %s\\n' \"$owner\" \"$management\" \"$container\"; \
         done"
    );
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)?;

    for line in result.stdout.lines() {
        if let Some(rest) = line.strip_prefix("RANGE ") {
            let mut fields = rest.split_whitespace();
            let owner = fields.next().unwrap_or("unknown");
            for value in fields {
                let Ok(cidr) = value.parse::<Ipv4Cidr>() else {
                    continue;
                };
                reject_project_range_overlap(server_name, owner, cidr, plan)?;
            }
            continue;
        }

        if line.starts_with("NETWORK ") {
            let mut fields = line.split_whitespace();
            let _prefix = fields.next();
            let name = fields.next().unwrap_or_default();
            for candidate in fields {
                let Ok(cidr) = candidate.parse::<Ipv4Cidr>() else {
                    continue;
                };
                if name == server_plan.bridge_name && cidr == server_plan.container_subnet {
                    continue;
                }
                if is_jiji_bridge_name(name) {
                    if cidr == server_plan.container_subnet {
                        reject_jiji_collision(
                            server_name,
                            "container subnet",
                            &cidr.to_string(),
                            name,
                        )?;
                    }
                    // A non-colliding jiji bridge from another project is the expected,
                    // supported case now -- not a conflict.
                    continue;
                }
                reject_overlap(server_name, "container network", cidr, plan)?;
            }
            continue;
        }

        if let Some(rest) = line.strip_prefix("PORT ") {
            let mut fields = rest.split_whitespace();
            let iface = fields.next().unwrap_or_default();
            let port = fields.next().and_then(|value| value.parse::<u16>().ok());
            if iface != server_plan.wireguard_interface && port == Some(server_plan.wireguard_port)
            {
                if is_jiji_managed_interface(iface) {
                    reject_jiji_collision(
                        server_name,
                        "WireGuard port",
                        &server_plan.wireguard_port.to_string(),
                        iface,
                    )?;
                } else {
                    anyhow::bail!(
                        "Server '{server_name}' already has WireGuard interface '{iface}' listening on port {}, which this project also needs. Free that port or reconfigure the other interface, then retry.",
                        server_plan.wireguard_port
                    );
                }
            }
            continue;
        }

        if let Some(rest) = line.strip_prefix("ADDR ") {
            let mut fields = rest.split_whitespace();
            let _index = fields.next();
            let iface = fields.next().unwrap_or_default();
            let is_inet = fields.next() == Some("inet");
            let address = fields.next().unwrap_or_default();
            if is_inet && iface != server_plan.wireguard_interface {
                if let Some((addr, _prefix)) = address.split_once('/') {
                    if addr.parse::<std::net::Ipv4Addr>().ok()
                        == Some(server_plan.management_address)
                    {
                        if is_jiji_managed_interface(iface) {
                            reject_jiji_collision(
                                server_name,
                                "WireGuard management address",
                                &server_plan.management_address.to_string(),
                                iface,
                            )?;
                        } else {
                            anyhow::bail!(
                                "Server '{server_name}' already has interface '{iface}' holding address {}, which this project's WireGuard peer also needs. Free that address or reconfigure the other interface, then retry.",
                                server_plan.management_address
                            );
                        }
                    }
                }
            }
            continue;
        }

        if line.contains(" dev jiji") {
            continue;
        }
        let Some(destination) = line.split_whitespace().next() else {
            continue;
        };
        let Ok(cidr) = destination.parse::<Ipv4Cidr>() else {
            continue;
        };
        reject_overlap(server_name, "host route", cidr, plan)?;
    }

    let bridge = BridgeProvisioner::new(engine, plan, server_plan);
    let migration = bridge.inspect_migration(session).await?;
    if migration.is_none() {
        let command = bridge.render_existing_validation_command();
        let result = session.execute(&command).await?;
        ensure_success(session, &command, &result)?;
    }
    Ok(migration)
}

fn reject_project_range_overlap(
    server_name: &str,
    owner: &str,
    existing: Ipv4Cidr,
    plan: &NetworkPlan,
) -> anyhow::Result<()> {
    if existing.overlaps(plan.management_cidr) || existing.overlaps(plan.container_cidr) {
        anyhow::bail!(
            "Server '{server_name}' has another jiji project ('{owner}') reserving '{existing}', which overlaps this project's planned management range '{}' or container range '{}'. Set explicit non-overlapping `network.management_cidr` and `network.container_cidr` values in jiji.yml, then retry.",
            plan.management_cidr,
            plan.container_cidr
        );
    }
    Ok(())
}

/// A collision between this project's planned resource and a resource that looks like it belongs
/// to a *different* jiji project sharing this host -- the rare-but-real hash collision case (see
/// the project's network-isolation design notes for the actual odds), not a foreign-infrastructure
/// conflict. States the real remedy plainly rather than implying this should basically never
/// happen.
fn reject_jiji_collision(
    server_name: &str,
    what: &str,
    value: &str,
    other_interface_or_network: &str,
) -> anyhow::Result<()> {
    anyhow::bail!(
        "Server '{server_name}' already has a jiji-managed resource ('{other_interface_or_network}') using {what} '{value}', which this project also needs. This is a collision between two independent jiji projects on the same host. Set distinct `network.container_cidr` and `network.management_cidr` values for one project in jiji.yml, then retry."
    );
}

fn reject_overlap(
    server_name: &str,
    source: &str,
    existing: Ipv4Cidr,
    plan: &NetworkPlan,
) -> anyhow::Result<()> {
    if existing.overlaps(plan.management_cidr) || existing.overlaps(plan.container_cidr) {
        anyhow::bail!(
            "Server '{server_name}' has an existing {source} '{existing}' that overlaps jiji's management range '{}' or container range '{}'. Change the jiji network CIDRs or remove the conflicting route, then retry.",
            plan.management_cidr,
            plan.container_cidr
        );
    }
    Ok(())
}

async fn install_prerequisites(session: &SshSession, slug: &str) -> anyhow::Result<()> {
    let project_dir = network_dir(slug);
    let command = format!(
        "set -eu; \
if command -v apt-get >/dev/null 2>&1; then \
  apt-get update -qq; \
  DEBIAN_FRONTEND=noninteractive apt-get install -y -qq wireguard-tools dnsutils iptables nftables; \
elif command -v dnf >/dev/null 2>&1; then \
  dnf install -y wireguard-tools bind-utils iptables nftables; \
else \
  echo 'Unsupported package manager. Install wireguard-tools, DNS query tools, iptables, and nftables manually.' >&2; exit 1; \
fi; \
install -d -m 0700 /etc/jiji/network; install -d -m 0700 {project_dir}; install -d -m 0700 /etc/wireguard; \
{}"
    , enable_linger_command());
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)
}

/// Without this, a rootful container (jiji-proxy, or any jiji-managed service container) started
/// by a command run over this SSH session can be killed by systemd once the session's own cgroup
/// scope is torn down at disconnect -- confirmed live on a stock Ubuntu 24.04 droplet, where
/// `jiji-proxy` died silently minutes after `jiji server setup` returned, with `podman`'s own
/// cached container state still (wrongly) reporting it as running. `loginctl enable-linger`
/// keeps the SSH user's systemd instance (and everything under it) running independently of any
/// login session. Best-effort: `loginctl` doesn't exist on every init system jiji might target,
/// so a missing binary must never fail the whole prerequisites step.
fn enable_linger_command() -> String {
    "command -v loginctl >/dev/null 2>&1 && loginctl enable-linger \"$(whoami)\" || true"
        .to_string()
}

async fn ensure_keypair(session: &SshSession, slug: &str) -> anyhow::Result<()> {
    let private_key_path = private_key_path(slug);
    let public_key_path = public_key_path(slug);
    let command = format!(
        "set -eu; umask 077; \
         test -s {private_key_path} || wg genkey > {private_key_path}; \
         wg pubkey < {private_key_path} > {public_key_path}; \
         chmod 0600 {private_key_path}; chmod 0644 {public_key_path}"
    );
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)
}

async fn ensure_engine_available(
    session: &SshSession,
    engine: ContainerEngine,
) -> anyhow::Result<()> {
    let command = format!("command -v {engine}");
    let result = session.execute(&command).await?;
    if !result.success {
        anyhow::bail!(
            "{} is not installed on {}. Run `jiji server setup` to install the engine and network together.",
            engine,
            session.host()
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn stage_host(
    session: &SshSession,
    engine: ContainerEngine,
    plan: &NetworkPlan,
    server: &ServerPlan,
    public_keys: &BTreeMap<String, String>,
    artifact_generation: &str,
    slug: &str,
    domains: ActivationDomains,
) -> anyhow::Result<()> {
    if domains.mesh {
        let bridge = BridgeProvisioner::new(engine, plan, server);
        let generation_dir = format!("{}/{artifact_generation}", network_generations(slug));
        let command = format!("install -d -m 0750 {generation_dir}");
        let result = session.execute(&command).await?;
        ensure_success(session, &command, &result)?;
        let wireguard = render_wireguard(server, public_keys)?;
        write_staged_file(
            session,
            &format!("{generation_dir}/wireguard.conf.input"),
            "0600",
            &wireguard,
        )
        .await?;
        let private_key_path = private_key_path(slug);
        // `wg-quick strip` requires its own filename (minus `.conf`) to itself be a valid interface
        // name (<=15 chars) -- `server.wireguard_interface` is already 12 chars, leaving no room for
        // any suffix, so this is a short, fixed, non-project-derived name instead (confirmed live: a
        // per-project staged name here made `wg-quick strip` fail outright). Matches the original
        // single-project code's behavior of using one shared transient staging path; two projects'
        // `network setup` runs racing on the exact same host at the exact same moment could stomp
        // this file, but that's the same class of accepted, out-of-scope concurrent-invocation risk
        // documented for the rest of this change, not a new one.
        let staged_wireguard_path = "/etc/wireguard/jiji-stage.conf";
        let finalize_wireguard = format!(
        "set -eu; private_key=$(cat {private_key_path}); \
         sed \"s|__JIJI_PRIVATE_KEY__|$private_key|\" {generation_dir}/wireguard.conf.input > {generation_dir}/wireguard.conf.new; \
         chmod 0600 {generation_dir}/wireguard.conf.new; \
         cp {generation_dir}/wireguard.conf.new {staged_wireguard_path}; \
         wg-quick strip {staged_wireguard_path} >/dev/null; \
         if test -e {generation_dir}/wireguard.conf; then \
           cmp -s {generation_dir}/wireguard.conf.new {generation_dir}/wireguard.conf || {{ echo 'Staged WireGuard content differs inside an existing immutable generation. Upgrade jiji or report an artifact-version bug.' >&2; exit 1; }}; \
           rm -f {generation_dir}/wireguard.conf.new; \
         else \
           mv {generation_dir}/wireguard.conf.new {generation_dir}/wireguard.conf; \
         fi; \
         rm -f {generation_dir}/wireguard.conf.input {staged_wireguard_path}"
    );
        let result = session.execute(&finalize_wireguard).await?;
        ensure_success(session, &finalize_wireguard, &result)?;

        // Legitimately host-global, not project-scoped: `net.ipv4.ip_forward=1` benefits every
        // project on the host, and `network_teardown`'s `remove_compiled_state` never removes it
        // (see that module's notes) rather than tracking multi-project reference counts for it.
        write_staged_file(
            session,
            "/etc/sysctl.d/99-jiji-network.conf",
            "0644",
            "net.ipv4.ip_forward=1\n",
        )
        .await?;
        write_generation_file(
            session,
            &format!("{generation_dir}/restore.sh"),
            "0750",
            &bridge.render_restore_script()?,
        )
        .await?;
        write_generation_file(
            session,
            &format!("{generation_dir}/address-ranges"),
            "0644",
            &format!("{} {}\n", plan.management_cidr, plan.container_cidr),
        )
        .await?;
        write_generation_file(
            session,
            &format!("{generation_dir}/mesh-generation"),
            "0644",
            &format!("{}\n", plan.mesh_generation),
        )
        .await?;
    }
    Ok(())
}

async fn write_staged_file(
    session: &SshSession,
    path: &str,
    mode: &str,
    content: &str,
) -> anyhow::Result<()> {
    let temporary = format!("{path}.jiji-new");
    let command =
        format!("set -eu; install -D -m {mode} /dev/stdin {temporary}; mv {temporary} {path}");
    let result = session
        .execute_with_input(&command, content.as_bytes())
        .await?;
    ensure_success(session, &command, &result)
}

async fn write_generation_file(
    session: &SshSession,
    path: &str,
    mode: &str,
    content: &str,
) -> anyhow::Result<()> {
    let temporary = format!("{path}.jiji-new");
    let command = format!(
        "set -eu; \
         install -D -m {mode} /dev/stdin {temporary}; \
         if test -e {path}; then \
           cmp -s {temporary} {path} || {{ echo 'Staged content differs inside an existing immutable generation. Upgrade jiji or report an artifact-version bug.' >&2; exit 1; }}; \
           rm -f {temporary}; \
         else \
           mv {temporary} {path}; \
         fi"
    );
    let result = session
        .execute_with_input(&command, content.as_bytes())
        .await?;
    ensure_success(session, &command, &result)
}

async fn capture_installed_generation(
    session: &SshSession,
    slug: &str,
) -> anyhow::Result<InstalledGeneration> {
    let current = network_current(slug);
    let command = format!(
        "set -eu; \
         if test -L {current}; then \
           readlink -f {current}; \
         else \
           printf '%s\\n' -; \
         fi"
    );
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)?;
    parse_installed_generation(&result.stdout, slug).with_context(|| {
        format!(
            "Host {} returned invalid installed network generation state",
            session.host()
        )
    })
}

fn parse_installed_generation(value: &str, slug: &str) -> anyhow::Result<InstalledGeneration> {
    let lines = value.lines().collect::<Vec<_>>();
    if lines.len() != 1 {
        anyhow::bail!("expected one generation path, received {}", lines.len());
    }
    let parse = |value: &str, root: &str| -> anyhow::Result<Option<String>> {
        if value == "-" {
            return Ok(None);
        }
        if !value.starts_with(root) || value.contains(char::is_whitespace) {
            anyhow::bail!("unsafe generation path '{value}'");
        }
        Ok(Some(value.to_string()))
    };
    Ok(InstalledGeneration {
        network: parse(lines[0], &format!("{}/", network_generations(slug)))?,
    })
}

async fn activate_host(
    session: &SshSession,
    server: &ServerPlan,
    mesh_generation: &str,
    artifact_generation: &str,
    slug: &str,
    activate_mesh: bool,
) -> anyhow::Result<()> {
    let command = render_activation_command(
        server,
        mesh_generation,
        artifact_generation,
        slug,
        activate_mesh,
    );
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)
}

/// Since Phase 9, nothing manages WireGuard routes on this interface but this codebase itself
/// (native `ip link`/`wg set` bring-up has no `wg-quick`-equivalent automatic route programming),
/// so both a fresh bring-up and a rollback's resync must apply this explicitly rather than relying
/// on a side effect of `wg-quick up`/`systemctl restart wg-quick@...` the way earlier phases did.
fn render_route_sync(server: &ServerPlan) -> String {
    let iface = &server.wireguard_interface;
    let expected_routes = server
        .peers
        .iter()
        .flat_map(|peer| peer.allowed_ips.iter())
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if expected_routes.is_empty() {
        format!(
            "for route in $(ip -4 route show dev {iface} | awk '{{print $1}}'); do ip route del \"$route\" dev {iface}; done"
        )
    } else {
        let replacements = expected_routes
            .iter()
            .map(|route| format!("ip route replace {route} dev {iface}"))
            .collect::<Vec<_>>()
            .join("; ");
        format!(
            "for route in $(ip -4 route show dev {iface} | awk '{{print $1}}'); do case \"$route\" in {}) ;; *) ip route del \"$route\" dev {iface} ;; esac; done; {replacements}",
            expected_routes.join("|")
        )
    }
}

fn render_activation_command(
    server: &ServerPlan,
    mesh_generation: &str,
    artifact_generation: &str,
    slug: &str,
    activate_mesh: bool,
) -> String {
    let iface = &server.wireguard_interface;
    let route_sync = render_route_sync(server);
    let network_generation = format!("{}/{artifact_generation}", network_generations(slug));
    let network_current = network_current(slug);
    let network_dir = network_dir(slug);
    let wireguard_config_path = wireguard_config_path(iface);
    let private_key_path = private_key_path(slug);
    let mesh_activation = if activate_mesh {
        format!(
            "sysctl --system >/dev/null; \
             test -s {network_generation}/wireguard.conf; \
             test -x {network_generation}/restore.sh; \
             test \"$(cat {network_generation}/mesh-generation)\" = '{mesh_generation}'; \
             ln -sfn {network_generation} {network_current}.new; \
             mv -Tf {network_current}.new {network_current}; \
             ln -sfn {network_current}/wireguard.conf {wireguard_config_path}.new; \
             mv -Tf {wireguard_config_path}.new {wireguard_config_path}; \
             ln -sfn {network_current}/restore.sh {network_dir}/restore.sh.new; \
             mv -Tf {network_dir}/restore.sh.new {network_dir}/restore.sh; \
             ln -sfn {network_current}/mesh-generation {network_dir}/mesh-generation.new; \
             mv -Tf {network_dir}/mesh-generation.new {network_dir}/mesh-generation; \
             if ! ip link show dev {iface} >/dev/null 2>&1; then ip link add {iface} type wireguard; fi; \
             wg set {iface} private-key {private_key_path} listen-port {wireguard_port}; \
             ip address replace {management}/32 dev {iface}; \
             ip link set {iface} up; \
             bash -c 'wg syncconf {iface} <(wg-quick strip {wireguard_config_path})'; \
             {route_sync}; \
             sh {network_dir}/restore.sh;",
            management = server.management_address,
            wireguard_port = server.wireguard_port,
        )
    } else {
        "true".to_string()
    };
    let mesh_activation = mesh_activation.trim().trim_end_matches(';');
    format!(
        "set -eu; \
         systemctl daemon-reload; \
         {mesh_activation}"
    )
}

struct RollbackContext<'a> {
    previous: &'a BTreeMap<String, InstalledGeneration>,
    activation_domains: &'a BTreeMap<String, ActivationDomains>,
    migrations: &'a BTreeMap<String, BridgeMigration>,
    plan: &'a NetworkPlan,
    engine: ContainerEngine,
}

async fn rollback_transaction(
    attempted: &[&ConnectedHost],
    context: &RollbackContext<'_>,
    failed_host: &str,
    phase: &str,
    cause: anyhow::Error,
) -> anyhow::Result<()> {
    Ui::error(&format!(
        "Network {phase} failed on {failed_host}: {cause}. Rolling back attempted hosts."
    ));
    let slug = jiji_network::systemd_unit_slug(&context.plan.project);
    let mut rollback_failures = Vec::new();
    for host in attempted.iter().rev() {
        let state = &context.previous[&host.name];
        let domains = context.activation_domains[&host.name];
        if let Some(migration) = context.migrations.get(&host.name) {
            let bridge = BridgeProvisioner::new(
                context.engine,
                context.plan,
                &context.plan.servers[&host.name],
            );
            if let Err(error) = bridge
                .restore_previous_bridge(&host.session, migration)
                .await
            {
                rollback_failures.push(format!(
                    "{}: could not restore previous bridge: {error}",
                    host.name
                ));
                continue;
            }
            if context.engine == ContainerEngine::Docker {
                if let Some(address) = migration.previous_proxy_address() {
                    let ingress_result =
                        match crate::proxy::parse_public_host(&context.plan.servers[&host.name]) {
                            Ok(public_host) => {
                                crate::proxy_ingress::ensure_ingress_rule(
                                    &host.session,
                                    address,
                                    public_host,
                                    true,
                                    &[],
                                )
                                .await
                            }
                            Err(error) => Err(error),
                        };
                    if let Err(error) = ingress_result {
                        rollback_failures.push(format!(
                            "{}: previous bridge was restored but proxy ingress was not: {error}",
                            host.name
                        ));
                        continue;
                    }
                }
            }
        }
        let rollback = rollback_host(
            &host.session,
            state,
            &slug,
            &context.plan.servers[&host.name],
            context.engine,
            &context.plan.project,
            domains,
        )
        .await;
        if let Err(error) = rollback {
            rollback_failures.push(format!("{}: {error}", host.name));
            continue;
        }
        Ui::say(
            &format!(
                "{}: restored generation {}",
                host.name,
                state.network_name()
            ),
            1,
        );
    }
    if rollback_failures.is_empty() {
        anyhow::bail!(
            "Network {phase} failed on '{failed_host}' and all attempted hosts were rolled back. Fix the reported error and rerun `jiji network setup`."
        );
    }
    anyhow::bail!(
        "Network {phase} failed on '{failed_host}'. Rollback also failed on: {}. Run `jiji network setup` after restoring SSH access, and inspect `{}` on those hosts.",
        rollback_failures.join("; "),
        network_current(&slug)
    )
}

async fn rollback_host(
    session: &SshSession,
    state: &InstalledGeneration,
    slug: &str,
    server: &ServerPlan,
    engine: ContainerEngine,
    project: &str,
    domains: ActivationDomains,
) -> anyhow::Result<()> {
    let command = render_rollback_command(state, slug, server, domains);
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)?;

    // Confirmed live: a first-install activation can fail *inside* `restore.sh` (run directly via
    // `sh {network_dir}/restore.sh`, no systemd unit involved since Phase 9) after it has already
    // created engine-level resources, including the bridge network.
    // `render_rollback_command` only reverts jiji's own compiled-state symlinks; it has no way to
    // know what a partially-run *external* script did. On a first install (no previous generation
    // to fall back to -- if there were, rerunning that generation's own idempotent `restore.sh`
    // already reconciles engine state correctly), also remove whatever engine resources this
    // attempt may have created, using the exact same primitives `jiji server teardown` uses, so a
    // failed `jiji network setup` never leaves orphaned infrastructure behind.
    if domains.mesh && state.network.is_none() {
        crate::network_teardown::remove_bridge_and_engine_network(session, engine, project)
            .await
            .with_context(|| {
                format!(
                    "rollback removed jiji's own compiled state on {}, but could not remove a partially-created bridge network",
                    session.host()
                )
            })?;
    }
    Ok(())
}

fn render_rollback_command(
    state: &InstalledGeneration,
    slug: &str,
    server: &ServerPlan,
    domains: ActivationDomains,
) -> String {
    let wireguard_interface = &server.wireguard_interface;
    let network_current = network_current(slug);
    let network_dir = network_dir(slug);
    let wireguard_config_path = wireguard_config_path(wireguard_interface);
    let network = if domains.mesh {
        match &state.network {
            Some(path) => format!(
            "ln -sfn {path} {network_current}.new; \
             mv -Tf {network_current}.new {network_current}; \
             ln -sfn {network_current}/wireguard.conf {wireguard_config_path}.new; \
             mv -Tf {wireguard_config_path}.new {wireguard_config_path}; \
             ln -sfn {network_current}/restore.sh {network_dir}/restore.sh.new; \
             mv -Tf {network_dir}/restore.sh.new {network_dir}/restore.sh; \
             ln -sfn {network_current}/mesh-generation {network_dir}/mesh-generation.new; \
             mv -Tf {network_dir}/mesh-generation.new {network_dir}/mesh-generation"
            ),
            // No previous generation ever existed, so this attempt's own bring-up (if it got that
            // far) is the only thing that could have created the interface -- tear it down rather
            // than leave a stray link with no compiled state pointing at it.
            None => format!(
            "ip link delete {wireguard_interface} 2>/dev/null || true; \
             rm -f {network_current} {wireguard_config_path} {network_dir}/restore.sh {network_dir}/mesh-generation"
            ),
        }
    } else {
        "true".to_string()
    };
    let restart_mesh = if domains.mesh && state.network.is_some() {
        // The interface itself was already up before this failed attempt began (a previous
        // generation exists); the private key and listen port are stable across a project's
        // generations (never regenerated once bootstrapped), so only the peer set and routes --
        // both generation-dependent -- need resyncing against the just-restored `wireguard.conf`.
        format!(
            "bash -c 'wg syncconf {wireguard_interface} <(wg-quick strip {wireguard_config_path})'; \
             {route_sync}; \
             sh {network_dir}/restore.sh",
            route_sync = render_route_sync(server),
        )
    } else {
        "true".to_string()
    };
    format!("set -eu; {network}; systemctl daemon-reload; {restart_mesh}")
}

impl InstalledGeneration {
    fn network_name(&self) -> &str {
        self.network
            .as_deref()
            .and_then(|path| path.rsplit('/').next())
            .unwrap_or("none")
    }
}

fn artifact_generation(plan_generation: &str, public_keys: &BTreeMap<String, String>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(NETWORK_ARTIFACT_VERSION.to_be_bytes());
    hasher.update((plan_generation.len() as u64).to_be_bytes());
    hasher.update(plan_generation.as_bytes());
    for (server, key) in public_keys {
        hasher.update((server.len() as u64).to_be_bytes());
        hasher.update(server.as_bytes());
        hasher.update((key.len() as u64).to_be_bytes());
        hasher.update(key.as_bytes());
    }
    let mut key_digest = String::with_capacity(16);
    for byte in &hasher.finalize()[..8] {
        write!(key_digest, "{byte:02x}").expect("writing to a String cannot fail");
    }
    format!("{plan_generation}-v{NETWORK_ARTIFACT_VERSION}-{key_digest}")
}

/// Best-effort: reads whichever peers' public keys `session`'s own currently-installed
/// `wireguard.conf` already recorded (from the last time it was configured with the full project
/// member set). Returns an empty map on any failure -- no previous generation installed, no
/// current symlink yet, an unreadable file -- since this is purely a fallback for filling gaps in
/// `apply_connected`'s own freshly-read keys, never a hard requirement.
async fn read_installed_peer_keys(session: &SshSession, slug: &str) -> BTreeMap<String, String> {
    let current = network_current(slug);
    let command = format!(
        "set -eu; if test -L {current}; then cat \"$(readlink -f {current})/wireguard.conf\" 2>/dev/null; fi"
    );
    match session.execute(&command).await {
        Ok(result) if result.success => parse_wireguard_peer_keys(&result.stdout),
        _ => BTreeMap::new(),
    }
}

/// Parses the `# {server}` / `PublicKey = {key}` pairs `render_wireguard` writes inside each
/// `[Peer]` block. Tolerant of anything else in the file (comments, blank lines, an `[Interface]`
/// section) -- only lines inside a `[Peer]` block are ever considered.
fn parse_wireguard_peer_keys(content: &str) -> BTreeMap<String, String> {
    let mut keys = BTreeMap::new();
    let mut current_name: Option<String> = None;
    let mut in_peer = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "[Peer]" {
            in_peer = true;
            current_name = None;
            continue;
        }
        if trimmed.starts_with('[') {
            in_peer = false;
            current_name = None;
            continue;
        }
        if !in_peer {
            continue;
        }
        if let Some(name) = trimmed.strip_prefix("# ") {
            current_name = Some(name.trim().to_string());
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("PublicKey = ") {
            if let Some(name) = current_name.take() {
                keys.insert(name, value.trim().to_string());
            }
        }
    }
    keys
}

async fn verify_host(
    session: &SshSession,
    server: &ServerPlan,
    plan: &NetworkPlan,
    domains: ActivationDomains,
    public_keys: &BTreeMap<String, String>,
) -> anyhow::Result<()> {
    let peer_checks = server
        .peers
        .iter()
        .filter(|peer| public_keys.contains_key(&peer.server))
        .map(|peer| {
            format!(
                "attempt=0; until ping -c 1 -W 3 {} >/dev/null; do attempt=$((attempt + 1)); [ \"$attempt\" -ge 5 ] && {{ echo \"WireGuard peer verification failed after 5 attempts: {} ({}) could not reach {} ({})\" >&2; exit 1; }}; sleep 1; done",
                peer.management_address,
                server.name,
                server.management_address,
                peer.server,
                peer.management_address,
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    let slug = jiji_network::systemd_unit_slug(&plan.project);
    let network_dir = network_dir(&slug);
    let mesh_check = if domains.mesh {
        format!(
            "wg show {wireguard_interface} >/dev/null; \
             ip link show {bridge_interface} >/dev/null; \
             ip -4 address show dev {bridge_interface} | grep -F '{bridge_gateway}' >/dev/null; \
             test \"$(cat {network_dir}/mesh-generation)\" = '{mesh_generation}'; \
             {peer_checks}",
            wireguard_interface = server.wireguard_interface,
            bridge_interface = server.bridge_interface,
            bridge_gateway = server.bridge_gateway,
            mesh_generation = plan.mesh_generation,
        )
    } else {
        "true".to_string()
    };
    let command = format!("set -eu; {mesh_check}");
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)
}

fn render_wireguard(
    server: &ServerPlan,
    public_keys: &BTreeMap<String, String>,
) -> anyhow::Result<String> {
    let mut output = format!(
        "[Interface]\nAddress = {}/32\nListenPort = {}\nPrivateKey = __JIJI_PRIVATE_KEY__\n",
        server.management_address, server.wireguard_port
    );
    for peer in &server.peers {
        let Some(public_key) = public_keys.get(&peer.server) else {
            continue;
        };
        output.push_str(&format!(
            "\n[Peer]\n# {}\nPublicKey = {}\nEndpoint = {}\nAllowedIPs = {}\nPersistentKeepalive = 25\n",
            peer.server,
            public_key,
            peer.endpoint,
            peer.allowed_ips
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(output)
}

fn ensure_success(
    session: &SshSession,
    command: &str,
    result: &CommandResult,
) -> anyhow::Result<()> {
    if result.success {
        return Ok(());
    }
    anyhow::bail!(
        "Command `{command}` failed on {} (exit {:?}): {}",
        session.host(),
        result.code,
        result.stderr.trim()
    )
}

async fn close_all(hosts: &[ConnectedHost]) {
    for host in hosts {
        host.session.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiji_config::Config;

    fn plan() -> NetworkPlan {
        let config: Config = serde_yaml::from_str(
            r#"
project: demo
builder: { engine: docker }
servers:
  app: { host: 203.0.113.10 }
  data: { host: 203.0.113.20 }
services:
  web: { servers: [app, data] }
  redis: { servers: [data] }
"#,
        )
        .unwrap();
        NetworkPlanner::new().plan(&config).unwrap()
    }

    #[test]
    fn wireguard_rendering_contains_only_peer_public_keys() {
        let plan = plan();
        let keys = BTreeMap::from([
            ("app".to_string(), "app-public-key".to_string()),
            ("data".to_string(), "data-public-key".to_string()),
        ]);
        let rendered = render_wireguard(&plan.servers["app"], &keys).unwrap();
        assert!(rendered.contains("PrivateKey = __JIJI_PRIVATE_KEY__"));
        assert!(rendered.contains("PublicKey = data-public-key"));
        assert!(!rendered.contains("app-public-key"));
        assert!(rendered.contains(&plan.servers["data"].container_subnet.to_string()));
    }

    #[test]
    fn parse_wireguard_peer_keys_recovers_exactly_what_render_wireguard_wrote() {
        // Round-trip regression guard for the targeted-`-H` peer-drop fix: an installed
        // `wireguard.conf` must yield back the same peer keys `render_wireguard` put there, since
        // `apply_connected` relies on parsing an existing host's config to fill gaps for project
        // members it didn't connect to this invocation.
        let plan = plan();
        let keys = BTreeMap::from([
            ("app".to_string(), "app-public-key".to_string()),
            ("data".to_string(), "data-public-key".to_string()),
        ]);
        // Rendered from "app"'s own perspective: only lists its peer ("data"), never itself.
        let rendered = render_wireguard(&plan.servers["app"], &keys).unwrap();
        let recovered = parse_wireguard_peer_keys(&rendered);
        assert_eq!(
            recovered.get("data").map(String::as_str),
            Some("data-public-key")
        );
        assert!(
            !recovered.contains_key("app"),
            "a host's own [Interface] section has no PublicKey line to mistake for a peer's"
        );
    }

    #[test]
    fn parse_wireguard_peer_keys_ignores_non_peer_sections_and_garbage() {
        let content = "[Interface]\n\
             Address = 198.18.1.10/32\n\
             PrivateKey = something\n\
             \n\
             [Peer]\n\
             # data\n\
             PublicKey = data-public-key\n\
             Endpoint = 203.0.113.20:51820\n\
             AllowedIPs = 198.18.1.20/32, 100.64.8.0/21\n\
             PersistentKeepalive = 25\n";
        let recovered = parse_wireguard_peer_keys(content);
        assert_eq!(recovered.len(), 1);
        assert_eq!(
            recovered.get("data").map(String::as_str),
            Some("data-public-key")
        );
    }

    #[test]
    fn combined_activation_never_contains_an_empty_command() {
        let plan = plan();
        let slug = jiji_network::systemd_unit_slug("demo");
        let command = render_activation_command(
            &plan.servers["app"],
            &plan.mesh_generation,
            "mesh-artifact",
            &slug,
            true,
        );
        assert!(!command.contains("; ;"), "command: {command}");
    }

    #[test]
    fn installed_generation_parser_rejects_paths_outside_managed_roots() {
        let slug = jiji_network::systemd_unit_slug("demo");
        let generations = network_generations(&slug);
        let state = parse_installed_generation(&format!("{generations}/abc\n"), &slug).unwrap();
        assert_eq!(state.network_name(), "abc");
        assert!(parse_installed_generation("/tmp/abc\n", &slug).is_err());
        assert!(parse_installed_generation(&format!("{generations}/abc extra\n"), &slug).is_err());
    }

    #[test]
    fn rollback_selects_the_previous_mesh_before_restarting_services() {
        let plan = plan();
        let server = &plan.servers["app"];
        let slug = jiji_network::systemd_unit_slug("demo");
        let wireguard_interface = &server.wireguard_interface;
        let state = InstalledGeneration {
            network: Some(format!("{}/old", network_generations(&slug))),
        };
        let command =
            render_rollback_command(&state, &slug, server, ActivationDomains { mesh: true });
        let network_switch = command
            .find(&format!("ln -sfn {}/old", network_generations(&slug)))
            .unwrap();
        let restart = command.find("wg syncconf").unwrap();
        assert!(network_switch < restart);
        assert!(command.contains(&format!(
            "ln -sfn {}/wireguard.conf {}.new",
            network_current(&slug),
            wireguard_config_path(wireguard_interface)
        )));
    }

    #[test]
    fn first_install_rollback_removes_only_jiji_managed_live_paths() {
        let plan = plan();
        let server = &plan.servers["app"];
        let slug = jiji_network::systemd_unit_slug("demo");
        let wireguard_interface = &server.wireguard_interface;
        let command = render_rollback_command(
            &InstalledGeneration { network: None },
            &slug,
            server,
            ActivationDomains { mesh: true },
        );
        assert!(command.contains(&format!("ip link delete {wireguard_interface}")));
        assert!(command.contains(&format!(
            "rm -f {} {} {}/restore.sh {}/mesh-generation",
            network_current(&slug),
            wireguard_config_path(wireguard_interface),
            network_dir(&slug),
            network_dir(&slug),
        )));
        assert!(!command.contains("wg syncconf"));
    }

    #[test]
    fn artifact_generation_is_stable_and_changes_when_a_public_key_rotates() {
        let keys = BTreeMap::from([
            ("app".to_string(), "app-key".to_string()),
            ("data".to_string(), "data-key".to_string()),
        ]);
        let first = artifact_generation("plan", &keys);
        assert_eq!(first, artifact_generation("plan", &keys));
        assert!(first.starts_with(&format!("plan-v{NETWORK_ARTIFACT_VERSION}-")));

        let mut rotated = keys;
        rotated.insert("data".to_string(), "rotated-key".to_string());
        assert_ne!(first, artifact_generation("plan", &rotated));
    }

    #[test]
    fn conflict_check_rejects_either_planned_range() {
        let plan = plan();
        assert!(reject_overlap("app", "route", plan.management_cidr, &plan).is_err());
        assert!(reject_overlap("app", "route", plan.container_cidr, &plan).is_err());
        assert!(reject_overlap("app", "route", "10.10.0.0/16".parse().unwrap(), &plan).is_ok());
    }

    #[test]
    fn project_range_marker_rejects_overlap_before_server_subnets_collide() {
        let plan = plan();
        let error =
            reject_project_range_overlap("app", "other-project", plan.container_cidr, &plan)
                .unwrap_err();
        assert!(error.to_string().contains("other-project"));
        assert!(error.to_string().contains("network.container_cidr"));
        assert!(reject_project_range_overlap(
            "app",
            "other-project",
            "10.10.0.0/16".parse().unwrap(),
            &plan,
        )
        .is_ok());
    }
}
