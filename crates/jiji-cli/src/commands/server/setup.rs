use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::io::IsTerminal;
use std::net::SocketAddr;
use std::sync::Arc;

use jiji_agent::membership::{
    MembershipRecord, MembershipScope, MembershipState, MembershipView,
    MEMBERSHIP_PROTOCOL_VERSION, MEMBERSHIP_SCHEMA_VERSION,
};
use jiji_agent::runtime::MeshConfig;
use jiji_config::{validate_config, Config, NamedServer, Ssh};
use jiji_network::{NetworkPlan, NetworkPlanner};
use jiji_ssh::{SshPool, SshSession};
use jiji_tui::Ui;

use crate::agent_distribution::{self, AgentBinarySource};
use crate::agent_install;
use crate::audit::{self, AuditStatus};
use crate::commands::network;
use crate::engine::{self, EngineStatus};
use crate::lock::{LockRequest, LockScope};
use crate::proxy::{self, ProxyStatus};
use crate::ssh_adapter;

const SSH_REFUSAL_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(31);

fn ssh_refusal_cooldown() -> std::time::Duration {
    if cfg!(debug_assertions) {
        if let Ok(milliseconds) = std::env::var("JIJI_TEST_SSH_REFUSAL_COOLDOWN_MS") {
            if let Ok(milliseconds) = milliseconds.parse() {
                return std::time::Duration::from_millis(milliseconds);
            }
        }
    }
    SSH_REFUSAL_COOLDOWN
}

/// UFW's `limit ssh` rule rejects a source after several connections in a 30-second window. A
/// teardown followed immediately by setup can reach that limit even when each command avoids
/// redundant sessions. Do not retry during the window because each rejected SYN refreshes it.
pub(crate) async fn connect_for_setup(
    options: &jiji_ssh::ConnectOptions,
) -> Result<SshSession, jiji_ssh::SshError> {
    match SshSession::connect(options).await {
        Err(error) if error.is_connection_refused() => {
            Ui::say(
                &format!(
                    "{}: SSH connection refused; waiting 31s before one retry",
                    options.host
                ),
                1,
            );
            tokio::time::sleep(ssh_refusal_cooldown()).await;
            SshSession::connect(options).await
        }
        result => result,
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    hosts: Option<&str>,
    yes: bool,
    rotate_key: bool,
    import: bool,
    import_dry_run: bool,
) -> anyhow::Result<()> {
    Ui::section("Server Setup:");
    let started_at = std::time::Instant::now();

    let start = std::env::current_dir()?;
    let config_path = config_file.map(std::path::Path::new);
    let (config, path) =
        crate::config_loading::load_config_for_ssh(environment, config_path, &start).await?;
    Ui::say(&format!("Configuration loaded from: {}", path.display()), 1);

    let validation = validate_config(&config);
    if !validation.valid {
        Ui::error(&format!(
            "Configuration validation failed with {} error(s):",
            validation.errors.len()
        ));
        for e in &validation.errors {
            Ui::say(&format!("{}: {}", e.path, e.message), 1);
        }
        anyhow::bail!("Configuration is invalid; fix the errors above and try again");
    }

    let ssh = config.ssh.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "No `ssh:` section configured in {}. Add at least `ssh.user:` before running server setup.",
            path.display()
        )
    })?;

    if config.servers.is_empty() {
        anyhow::bail!(
            "No servers defined in {}. Add a `servers:` entry before running server setup.",
            path.display()
        );
    }

    let network_plan = NetworkPlanner::new()
        .plan(&config)
        .map_err(|error| anyhow::anyhow!("Could not build the private network plan: {error}"))?;

    let host_filters = split_comma_trimmed(hosts);
    let mut target_names = BTreeSet::new();
    for filter in &host_filters {
        match network_plan.select_hosts(std::slice::from_ref(filter)) {
            Ok(matches) => {
                target_names.extend(matches.into_iter().map(|server| server.name.clone()))
            }
            Err(_) => Ui::warn(&format!(
                "Host filter '{filter}' matched no servers; continuing with the other filters."
            )),
        }
    }
    if host_filters.is_empty() {
        target_names.extend(network_plan.servers.keys().cloned());
    } else if target_names.is_empty() {
        anyhow::bail!("No host filters matched a configured server.");
    }

    let mut servers: Vec<(String, NamedServer)> = config
        .servers
        .iter()
        .filter(|(name, _)| target_names.contains(*name))
        .map(|(name, server)| (name.clone(), server.clone()))
        .collect();
    servers.sort_by(|a, b| a.0.cmp(&b.0));

    Ui::say(
        &format!(
            "Targeting {} server(s): {}",
            servers.len(),
            servers
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        1,
    );

    // Live per-host dashboard — one overall bar + one row per host (TTY: MultiProgress,
    // non-TTY: plain lines). Stays alive through the whole locked section.
    let host_names: Vec<String> = servers.iter().map(|(name, _)| name.clone()).collect();
    let setup_progress = Ui::server_setup_progress(host_names);
    let setup_handle = setup_progress.handle();

    if rotate_key {
        Ui::warn(&format!(
            "--rotate-key will force a fresh WireGuard keypair on: {}",
            servers
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        Ui::say(
            "Every peer will briefly lose connectivity to each rotated host until it picks up the new key on its next reconcile.",
            1,
        );
        if !yes {
            if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
                anyhow::bail!(
                    "Refusing to prompt for confirmation without a terminal attached. Pass --yes to confirm the key rotation when running non-interactively (e.g. CI/CD)."
                );
            }
            let confirmed =
                Ui::confirm("Proceed with rotating these hosts' WireGuard keys?", false)?;
            if !confirmed {
                anyhow::bail!("Server setup cancelled: key rotation was not confirmed.");
            }
        }
    }

    // A dedicated connection purely to hold the host-runtime lock: unlike `deploy`, each phase
    // below (engine install, network setup, proxy, agent install) already manages its own
    // independent connect/close cycle, so there is no single persistent session set to reuse here.
    let lock_pool = SshPool::new(ssh.max_concurrent_starts as usize);
    let mut lock_connect_operations = Vec::with_capacity(servers.len());
    for (name, server) in &servers {
        lock_connect_operations.push(ssh_adapter::connect_options(name, server, &ssh)?);
    }
    let operations: Vec<_> = lock_connect_operations
        .into_iter()
        .map(|options| move || async move { connect_for_setup(&options).await })
        .collect();
    let connections = lock_pool.execute_concurrent(operations).await;
    let mut lock_sessions: BTreeMap<String, Arc<SshSession>> = BTreeMap::new();
    let mut lock_failures = Vec::new();
    for ((name, server), connection) in servers.iter().zip(connections) {
        match connection {
            Ok(session) => {
                lock_sessions.insert(name.clone(), Arc::new(session));
            }
            Err(err) => lock_failures.push((name.clone(), server.host.clone(), err.to_string())),
        }
    }
    if !lock_failures.is_empty() {
        close_all(&lock_sessions).await;
        anyhow::bail!(
            "Could not connect to server(s): {}. Restore SSH access and retry.",
            lock_failures
                .iter()
                .map(|(name, host, error)| format!("{name} ({host}): {error}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let lock_requests: Vec<LockRequest> = servers
        .iter()
        .map(|(name, _)| LockRequest::new(LockScope::HostRuntime, name.clone()))
        .collect();

    // Clone handle for the locked section — `with_locks` takes `Fn` so we clone inside.
    let setup_handle_for_locks = setup_handle.clone();
    let setup_result = crate::commands::lock::with_locks(
        &lock_pool,
        &lock_sessions,
        &config.project,
        lock_requests,
        format!(
            "jiji server setup: {}",
            servers
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        crate::commands::lock::AutomaticLockOptions {
            timeout: 300,
            force: false,
        },
        || {
            let setup_handle = setup_handle_for_locks.clone();
            let servers = servers.clone();
            let ssh = ssh.clone();
            let config = config.clone();
            let network_plan = network_plan.clone();
            let target_names = target_names.clone();
            let path = path.clone();
            let lock_sessions = lock_sessions.clone();
            async move {
                let pool = SshPool::new(ssh.max_concurrent_starts as usize);
                let mut connect_operations = Vec::with_capacity(servers.len());
                for (name, server) in &servers {
                    connect_operations.push(ssh_adapter::connect_options(name, server, &ssh)?);
                }

                Ui::section("Connecting:");
                for (name, _) in &servers {
                    setup_handle.set_status(name, "connecting");
                }
                let operations: Vec<_> = connect_operations
                    .into_iter()
                    .map(|options| move || async move { connect_for_setup(&options).await })
                    .collect();
                let connections = pool.execute_concurrent(operations).await;

                let mut failures: Vec<(String, String)> = Vec::new();
                let mut sessions: Vec<(String, SshSession)> = Vec::new();
                for ((name, server), connection) in servers.iter().zip(connections) {
                    match connection {
                        Ok(session) => {
                            Ui::say(&format!("{name} ({}): connected", server.host), 1);
                            setup_handle.set_status(name, "connected");
                            sessions.push((name.clone(), session));
                        }
                        Err(err) => {
                            Ui::error(&format!("{name} ({}): {err}", server.host));
                            setup_handle.set_status(name, &format!("failed: {err}"));
                            failures.push((name.clone(), err.to_string()));
                        }
                    }
                }

                if sessions.is_empty() {
                    anyhow::bail!("Could not connect to any server; see the errors above");
                }

                Ui::section("Installing Container Engine:");
                let engine = config.builder.engine;
                for (name, session) in &sessions {
                    Ui::say(&format!("{name} ({}):", session.host()), 1);
                    setup_handle.set_status(name, &format!("engine: checking {engine}"));
                    match engine::ensure_engine(session, engine).await {
                        Ok(EngineStatus::AlreadyInstalled(version)) => {
                            Ui::say(&format!("{engine} already installed ({version})"), 2);
                            setup_handle
                                .set_status(name, &format!("engine: {engine} already installed"));
                        }
                        Ok(EngineStatus::Installed(version)) => {
                            Ui::say(&format!("{engine} installed ({version})"), 2);
                            setup_handle.set_status(name, &format!("engine: {engine} installed"));
                        }
                        Ok(EngineStatus::Upgraded { from, to }) => {
                            Ui::say(&format!("{engine} upgraded ({from} -> {to})"), 2);
                            setup_handle.set_status(name, &format!("engine: {engine} upgraded"));
                        }
                        Err(err) => {
                            Ui::error(&format!("  {err}"));
                            setup_handle.set_status(name, &format!("engine failed: {err}"));
                            failures.push((name.clone(), err.to_string()));
                        }
                    }
                    session.close().await;
                }

                if !failures.is_empty() {
                    Ui::error(&format!("\n{} server(s) failed:", failures.len()));
                    for (name, message) in &failures {
                        Ui::say(&format!("{name}: {message}"), 1);
                        setup_handle.mark_failed(name, message);
                    }
                    anyhow::bail!("Server setup failed for {} server(s)", failures.len());
                }

                if rotate_key {
                    for (name, _) in &servers {
                        setup_handle.set_status(name, "rotating WireGuard keys");
                    }
                    force_rotate_keys(&servers, &config, &ssh).await?;
                    for (name, _) in &servers {
                        setup_handle.set_status(name, "keys rotated");
                    }
                }

                for (name, _) in &servers {
                    setup_handle.set_status(name, "network: reconciling");
                }
                network::setup::reconcile_for_server_setup(&config, &network_plan, &target_names)
                    .await
                    .map_err(|error| {
                        anyhow::anyhow!(
                "Container engine setup succeeded, but complete network setup failed: {error}"
            )
                    })?;
                for (name, _) in &servers {
                    setup_handle.set_status(name, "network: ready");
                }

                for (name, _) in &servers {
                    setup_handle.set_status(name, "agent: installing");
                }
                setup_agents(
                    &config,
                    &path,
                    &network_plan,
                    &servers,
                    &ssh,
                    yes,
                    &lock_sessions,
                    Some(setup_handle.clone()),
                )
                .await?;
                for (name, _) in &servers {
                    // Agent phase done — proxy is next; keep dashboard moving.
                    setup_handle.set_status(name, "agent: ready");
                }

                if import {
                    for (name, _) in &servers {
                        setup_handle.set_status(name, "import: checking");
                    }
                    perform_import(&config, &servers, &network_plan, &ssh, import_dry_run, yes)
                        .await?;
                    for (name, _) in &servers {
                        setup_handle.set_status(name, "import: done");
                    }
                }

                for (name, _) in &servers {
                    setup_handle.set_status(name, "proxy: configuring");
                }
                setup_proxies(
                    &config,
                    &servers,
                    &ssh,
                    started_at,
                    Some(setup_handle.clone()),
                )
                .await?;
                let replayed = crate::commands::network::backup::replay_recovery_desired_state(
                    &path, &config, &servers, &ssh,
                )
                .await?;
                if replayed > 0 {
                    Ui::result_ok(
                        "recovery",
                        &format!(
                        "restored desired placement for {replayed} service(s) into the new epoch"
                    ),
                    );
                }

                Ok(())
            }
        },
    )
    .await;
    close_all(&lock_sessions).await;
    // Always finish dashboard before returning — even on error, so the live bars
    // don't interleave with the final error output.
    setup_progress.finish();
    setup_result?;

    Ui::success_elapsed("All servers are ready.", started_at.elapsed());
    Ok(())
}

async fn close_all(sessions: &BTreeMap<String, Arc<SshSession>>) {
    for session in sessions.values() {
        session.close().await;
    }
}

/// Forces a fresh WireGuard keypair on every targeted host, bypassing `ensure_keypair`'s normal
/// idempotency guard (`test -s ... ||`). Only ever called for `--rotate-key`'s explicit targets --
/// never for an incidental peer this run happens to also connect to. The freshly minted public key
/// is what `setup_agents`'s Pass 1 reads back moments later, which is what actually fences the old
/// identity out of the mesh via `membership::reconcile_record`.
async fn force_rotate_keys(
    servers: &[(String, NamedServer)],
    config: &Config,
    ssh: &Ssh,
) -> anyhow::Result<()> {
    Ui::section("Rotating WireGuard Keys:");
    let slug = jiji_network::systemd_unit_slug(&config.project);
    let private_key_path = network::setup::private_key_path(&slug);
    let public_key_path = network::setup::public_key_path(&slug);
    let pool = SshPool::new(ssh.max_concurrent_starts as usize);
    let mut connect_operations = Vec::with_capacity(servers.len());
    for (name, server) in servers {
        connect_operations.push(ssh_adapter::connect_options(name, server, ssh)?);
    }
    let operations: Vec<_> = connect_operations
        .into_iter()
        .map(|options| move || async move { connect_for_setup(&options).await })
        .collect();
    let connections = pool.execute_concurrent(operations).await;

    let mut failures = Vec::new();
    for ((name, _), connection) in servers.iter().zip(connections) {
        let session = match connection {
            Ok(session) => session,
            Err(error) => {
                failures.push((name.clone(), error.to_string()));
                continue;
            }
        };
        let command = format!(
            "set -eu; umask 077; rm -f {private_key_path} {public_key_path}; \
             wg genkey > {private_key_path}; wg pubkey < {private_key_path} > {public_key_path}; \
             chmod 0600 {private_key_path}; chmod 0644 {public_key_path}"
        );
        let result = session.execute(&command).await;
        session.close().await;
        match result {
            Ok(result) if result.success => {
                Ui::result_ok(name, "WireGuard keypair rotated");
            }
            Ok(result) => failures.push((name.clone(), result.stderr.trim().to_string())),
            Err(error) => failures.push((name.clone(), error.to_string())),
        }
    }

    if !failures.is_empty() {
        for (name, error) in &failures {
            Ui::say(&format!("{name}: {error}"), 1);
        }
        anyhow::bail!(
            "Key rotation failed for {} server(s). Fix the reported hosts and retry `jiji server setup --rotate-key`.",
            failures.len()
        );
    }
    Ok(())
}

/// Runs after `setup_agents`, since importing a replica's catalog history needs the target
/// host's agent already up and reachable over its own live socket API. Uses its own connect
/// cycle (the sessions `setup_agents` opened are already closed by the time it returns) and
/// relies on the `HostRuntime` locks `run` already holds for `servers` -- no separate lock.
async fn perform_import(
    config: &Config,
    servers: &[(String, NamedServer)],
    network_plan: &NetworkPlan,
    ssh: &Ssh,
    dry_run: bool,
    yes: bool,
) -> anyhow::Result<()> {
    let pool = SshPool::new(ssh.max_concurrent_starts as usize);
    let mut connect_operations = Vec::with_capacity(servers.len());
    for (name, server) in servers {
        connect_operations.push(ssh_adapter::connect_options(name, server, ssh)?);
    }
    let operations: Vec<_> = connect_operations
        .into_iter()
        .map(|options| move || async move { connect_for_setup(&options).await })
        .collect();
    let connections = pool.execute_concurrent(operations).await;

    let mut sessions: BTreeMap<String, Arc<SshSession>> = BTreeMap::new();
    let mut failures = Vec::new();
    for ((name, _), connection) in servers.iter().zip(connections) {
        match connection {
            Ok(session) => {
                sessions.insert(name.clone(), Arc::new(session));
            }
            Err(error) => failures.push((name.clone(), error.to_string())),
        }
    }
    if !failures.is_empty() {
        for session in sessions.values() {
            session.close().await;
        }
        anyhow::bail!(
            "Could not connect to server(s) for import: {}",
            failures
                .iter()
                .map(|(name, error)| format!("{name}: {error}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let result = network::import::run_import(config, &sessions, network_plan, dry_run, yes).await;
    for session in sessions.values() {
        session.close().await;
    }
    result
}

/// Gathers this project's replicated membership -- once through each of `lock_sessions`'
/// already-open connections (no extra SSH handshake, and stays below common `ufw limit ssh`
/// thresholds), once via `membership::gather_membership` against the full configured set -- and
/// folds every record into a fresh `MembershipView`. Shared by `server setup` (which may then
/// tombstone decommissioned servers against this same view before enrolling -- see
/// `setup_agents`) and `server upgrade` (`upgrade::upgrade_agents`, which never decommissions
/// anything, so it uses this view as-is).
pub(crate) async fn gather_membership_view(
    config: &Config,
    ssh: &Ssh,
    lock_sessions: &BTreeMap<String, Arc<SshSession>>,
    target_names: &BTreeSet<String>,
    scope: &MembershipScope,
) -> MembershipView {
    let mut gathered_membership = Vec::new();
    for (name, session) in lock_sessions {
        if !target_names.contains(name) {
            continue;
        }
        if let Ok(records) =
            crate::commands::network::membership::pull_membership(session, &config.project).await
        {
            gathered_membership.extend(records);
        }
    }
    gathered_membership.extend(
        crate::commands::network::membership::gather_membership(config, ssh, target_names).await,
    );

    let mut view = MembershipView::default();
    for record in gathered_membership {
        // A stale/superseded record from a lagging peer is expected and harmless; only a
        // structural problem (wrong project, collision) is worth surfacing here.
        if let Err(error) = view.apply(record, scope) {
            Ui::warn(&format!(
                "Ignoring an inconsistent gathered membership record: {error}"
            ));
        }
    }
    view
}

/// Pass 1 of agent enrollment, shared by `server setup` and `server upgrade`: connects to every
/// target, reads its own WireGuard public key, reconciles a fresh candidate record into `view`,
/// and builds its `MeshConfig` -- everything `agent_install::ensure_agent` needs. Returns each
/// target's still-open session (so the caller can install without reconnecting, i.e. "Pass 2"),
/// the final `records` set (every target's own record plus everything already in `view` -- what
/// each enrolled host bootstraps with, so a freshly enrolled host and every existing peer agree
/// from the start, with no gossip left to converge them afterward), and any per-host failures
/// (connect refused, missing WireGuard key, an inconsistent reconciled record).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn enroll_agent_targets(
    config: &Config,
    network_plan: &NetworkPlan,
    servers: &[(String, NamedServer)],
    ssh: &Ssh,
    recovery_epoch: u64,
    scope: &MembershipScope,
    mut view: MembershipView,
) -> anyhow::Result<(
    Vec<(String, SshSession, MeshConfig)>,
    Vec<MembershipRecord>,
    Vec<(String, String)>,
)> {
    let pool = SshPool::new(ssh.max_concurrent_starts as usize);
    let mut connect_operations = Vec::with_capacity(servers.len());
    for (name, server) in servers {
        connect_operations.push(ssh_adapter::connect_options(name, server, ssh)?);
    }
    let operations: Vec<_> = connect_operations
        .into_iter()
        .map(|options| move || async move { connect_for_setup(&options).await })
        .collect();
    let connections = pool.execute_concurrent(operations).await;

    let mut failures = Vec::new();
    let mut prepared = Vec::new();
    for ((name, _), connection) in servers.iter().zip(connections) {
        let session = match connection {
            Ok(session) => session,
            Err(error) => {
                failures.push((name.clone(), error.to_string()));
                continue;
            }
        };
        let server_plan = &network_plan.servers[name];
        let slug = jiji_network::systemd_unit_slug(&config.project);
        let key_result = session
            .execute(&format!("cat {}", network::setup::public_key_path(&slug)))
            .await?;
        if !key_result.success || key_result.stdout.trim().is_empty() {
            failures.push((
                name.clone(),
                "WireGuard public key is unavailable after network bootstrap".into(),
            ));
            session.close().await;
            continue;
        }
        let endpoint: SocketAddr =
            format!("{}:{}", server_plan.public_host, server_plan.wireguard_port)
                .parse()
                .map_err(|_| {
                    anyhow::anyhow!(
                        "Server '{}' host '{}' must be a public IP address for mesh enrollment",
                        name,
                        server_plan.public_host
                    )
                })?;
        let candidate = MembershipRecord {
            project_id: config.project.clone(),
            recovery_epoch,
            protocol_version: MEMBERSHIP_PROTOCOL_VERSION,
            schema_version: MEMBERSHIP_SCHEMA_VERSION,
            node_id: name.clone(),
            server_name: name.clone(),
            wireguard_public_key: key_result.stdout.trim().to_string(),
            management_address: server_plan.management_address,
            container_subnet: server_plan.container_subnet.to_string(),
            endpoints: vec![endpoint],
            // Placeholder: `reconcile_record` decides the real owner_epoch/revision/state below,
            // by comparing this candidate's key/endpoint against `view.get(name)`.
            owner_epoch: 1,
            revision: 1,
            state: MembershipState::Active,
        };
        let reconciled =
            crate::commands::network::membership::reconcile_record(view.get(name), candidate);
        if let Some(record) = reconciled {
            if let Err(error) = view.apply(record, scope) {
                failures.push((
                    name.clone(),
                    format!("could not enroll membership: {error}"),
                ));
                session.close().await;
                continue;
            }
        }
        let peer_public_ips = network::bridge::BridgeProvisioner::new(
            config.builder.engine,
            network_plan,
            server_plan,
        )
        .peer_public_ips()?;
        let mut proxy_routes = Vec::new();
        let mut tcp_routes = Vec::new();
        for (service_name, service) in &config.services {
            if !service.servers.contains(name) {
                continue;
            }
            let Some(proxy) = service.proxy.as_ref() else {
                continue;
            };
            proxy_routes.extend(crate::proxy_routes::runtime_specs_for_service(
                &config.project,
                service_name,
                Some(proxy),
            )?);
            tcp_routes.extend(crate::proxy_routes::runtime_tcp_specs_for_service(
                &config.project,
                service_name,
                Some(proxy),
            )?);
        }
        let mesh_config = MeshConfig {
            project_id: config.project.clone(),
            recovery_epoch,
            node_id: name.clone(),
            wireguard_interface: server_plan.wireguard_interface.clone(),
            wireguard_private_key_path: network::setup::private_key_path(&slug).into(),
            replication_bind: SocketAddr::new(
                server_plan.management_address.into(),
                jiji_network::catalog_replication_port(&config.project),
            ),
            dns_bind_address: server_plan.dns_address,
            local_runtime: jiji_agent::runtime::LocalRuntimeConfig {
                bridge_network: server_plan.bridge_name.clone(),
                bridge_interface: server_plan.bridge_interface.clone(),
                proxy_address: server_plan.proxy_address,
                proxy_routes,
                tcp_routes,
                container_subnet: server_plan.container_subnet,
                bridge_gateway: server_plan.bridge_gateway,
                container_cidr: network_plan.container_cidr,
                wireguard_port: server_plan.wireguard_port,
                peer_public_ips,
                public_host: server_plan.public_host.clone(),
            },
            reconcile_interval_secs: 10,
            store_soft_quota_bytes: jiji_agent::runtime::DEFAULT_STORE_SOFT_QUOTA_BYTES,
            compaction_interval_secs: jiji_agent::runtime::DEFAULT_COMPACTION_INTERVAL_SECS,
            dns_forwarders: config
                .network
                .as_ref()
                .map(|network| network.dns_forwarders.clone())
                .unwrap_or_else(jiji_config::default_dns_forwarders),
        };
        prepared.push((name.clone(), session, mesh_config));
    }

    // The complete set (every target's own record plus everything gathered from already-enrolled
    // peers) is what each host bootstraps with, so a freshly enrolled host and every existing peer
    // agree from the start -- there is no gossip left to converge them afterward.
    let records: Vec<MembershipRecord> = view.all().cloned().collect();

    Ok((prepared, records, failures))
}

#[allow(clippy::too_many_arguments)]
async fn setup_agents(
    config: &Config,
    config_path: &std::path::Path,
    network_plan: &NetworkPlan,
    servers: &[(String, NamedServer)],
    ssh: &Ssh,
    yes: bool,
    lock_sessions: &BTreeMap<String, Arc<SshSession>>,
    progress: Option<jiji_tui::ServerSetupProgressHandle>,
) -> anyhow::Result<()> {
    // Local discovery (env override, sibling binary) still wins; otherwise fall back to a
    // host-side install script that downloads the matching-version agent from the GitHub
    // release, verified by sha256, on each remote host being set up.
    let (binary_source, remote_install_script) = agent_distribution::resolve_agent_binary_source(
        &config.project,
        "Phase 3 requires the authoritative jiji agent",
        |version| {
            format!(
                "No local jiji-agent binary found; installing jiji-agent v{version} from the \
                 release on each server."
            )
        },
    )
    .await?;
    let target_names: BTreeSet<String> = servers.iter().map(|(name, _)| name.clone()).collect();
    Ui::section("Installing Jiji Agent:");

    let recovery_epoch = crate::recovery_epoch::read(config_path)?;
    let scope = MembershipScope::new(config.project.clone(), recovery_epoch);
    let mut view = gather_membership_view(config, ssh, lock_sessions, &target_names, &scope).await;

    // A server still Active in the gathered mesh view but no longer present in `servers:` was
    // deliberately removed from config -- tombstone it. Driven off the full configured set, not
    // this run's `-H`-filtered targets, so a server that's merely offline or unselected this run
    // is never mistaken for one that was actually removed.
    let configured: BTreeSet<String> = config.servers.keys().cloned().collect();
    let decommissions =
        crate::commands::network::membership::compute_decommissions(&configured, &view);
    if !decommissions.is_empty() {
        let names: Vec<&str> = decommissions
            .iter()
            .map(|record| record.server_name.as_str())
            .collect();
        Ui::warn(&format!(
            "The following server(s) are no longer in `servers:` and will be permanently removed \
             from the mesh: {}",
            names.join(", ")
        ));
        if !yes {
            if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
                anyhow::bail!(
                    "Refusing to prompt for confirmation without a terminal attached. Pass --yes to confirm decommissioning these hosts when running non-interactively (e.g. CI/CD)."
                );
            }
            let confirmed = Ui::confirm("Permanently remove these servers from the mesh?", false)?;
            if !confirmed {
                anyhow::bail!("Server setup cancelled: decommissioning was not confirmed.");
            }
        }
        for record in decommissions {
            if let Err(error) = view.apply(record, &scope) {
                Ui::warn(&format!(
                    "Ignoring an inconsistent decommission record: {error}"
                ));
            }
        }
    }

    let (prepared, records, mut failures) = enroll_agent_targets(
        config,
        network_plan,
        servers,
        ssh,
        recovery_epoch,
        &scope,
        view,
    )
    .await?;

    // Pass 2: install, now that every target's record is folded into `records`.
    for (name, session, mesh_config) in prepared {
        let binary_path = match &binary_source {
            AgentBinarySource::Local(path) => Some(path.clone()),
            AgentBinarySource::Managed(_) => {
                let script = remote_install_script
                    .as_ref()
                    .expect("remote script is rendered whenever managed mode is used");
                let result = session.execute(script).await?;
                if !result.success {
                    failures.push((
                        name.clone(),
                        format!(
                            "could not download the jiji agent from the release: {}",
                            result.stderr.trim()
                        ),
                    ));
                    session.close().await;
                    continue;
                }
                None
            }
        };
        if let Some(handle) = &progress {
            handle.set_status(&name, "agent: installing");
        }
        let agent_ready = match agent_install::ensure_agent(
            &session,
            config.builder.engine,
            &config.project,
            binary_path.as_deref(),
            &mesh_config,
            &records,
        )
        .await
        {
            Ok(agent_install::AgentStatus::AlreadyRunning) => {
                Ui::say(&format!("{name}: jiji agent already running"), 1);
                if let Some(handle) = &progress {
                    handle.set_status(&name, "agent: already running");
                }
                true
            }
            Ok(agent_install::AgentStatus::Installed) => {
                Ui::say(&format!("{name}: jiji agent installed and running"), 1);
                if let Some(handle) = &progress {
                    handle.set_status(&name, "agent: installed");
                }
                true
            }
            Ok(agent_install::AgentStatus::Upgraded) => {
                Ui::say(&format!("{name}: jiji agent binary upgraded"), 1);
                if let Some(handle) = &progress {
                    handle.set_status(&name, "agent: upgraded");
                }
                true
            }
            Err(error) => {
                Ui::error(&format!("{name}: {error}"));
                if let Some(handle) = &progress {
                    handle.set_status(&name, &format!("agent failed: {error}"));
                }
                failures.push((name.clone(), error.to_string()));
                false
            }
        };
        if agent_ready {
            if let Err(error) = crate::commands::network::membership::push_membership(
                &session,
                &config.project,
                &records,
            )
            .await
            {
                Ui::error(&format!("{name}: {error}"));
                if let Some(handle) = &progress {
                    handle.set_status(&name, &format!("membership failed: {error}"));
                }
                failures.push((name.clone(), error.to_string()));
            } else if let Some(handle) = &progress {
                handle.set_status(&name, "agent: membership pushed");
            }
        }
        session.close().await;
    }

    if !failures.is_empty() {
        for (name, error) in &failures {
            Ui::say(&format!("{name}: {error}"), 1);
            if let Some(handle) = &progress {
                handle.mark_failed(name, error);
            }
        }
        anyhow::bail!(
            "Jiji agent install failed for {} server(s). Fix the reported hosts and retry `jiji server setup`.",
            failures.len()
        );
    }

    // Each target received `records` through its still-open install session above. Membership
    // must also reach any already-set-up server outside this
    // run's target set, or its own WireGuard reconciliation would never learn about the change --
    // there is no peer-to-peer membership relay to paper over the gap (see
    // `jiji_agent::membership`). Best-effort: an unreachable host just catches up next time any
    // command reaches it.
    let push_outcome = crate::commands::network::membership::push_membership_everywhere(
        config,
        ssh,
        &records,
        &target_names,
    )
    .await?;
    for (name, error) in &push_outcome.unreachable {
        Ui::say(&format!("{name}: membership not yet current ({error})"), 1);
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn setup_proxies(
    config: &Config,
    servers: &[(String, NamedServer)],
    ssh: &Ssh,
    started_at: std::time::Instant,
    progress: Option<jiji_tui::ServerSetupProgressHandle>,
) -> anyhow::Result<()> {
    let plan = NetworkPlanner::new()
        .plan(config)
        .map_err(|error| anyhow::anyhow!("Could not build the proxy network plan: {error}"))?;
    let dns_enabled = plan.enabled;

    Ui::section("Configuring Jiji Proxy:");
    let pool = SshPool::new(ssh.max_concurrent_starts as usize);
    let mut connect_operations = Vec::with_capacity(servers.len());
    for (name, server) in servers {
        connect_operations.push(ssh_adapter::connect_options(name, server, ssh)?);
    }
    let operations: Vec<_> = connect_operations
        .into_iter()
        .map(|options| move || async move { connect_for_setup(&options).await })
        .collect();
    let connections = pool.execute_concurrent(operations).await;

    let mut failures = Vec::new();
    for ((name, _), connection) in servers.iter().zip(connections) {
        let session = match connection {
            Ok(session) => session,
            Err(error) => {
                if let Some(handle) = &progress {
                    handle.set_status(name, &format!("proxy failed: {error}"));
                    handle.mark_failed(name, &error.to_string());
                }
                failures.push((name.clone(), error.to_string()));
                continue;
            }
        };
        if let Some(handle) = &progress {
            handle.set_status(name, "proxy: configuring");
        }
        let server_plan = &plan.servers[name];
        let network = if dns_enabled {
            Some(proxy::ProxyNetwork {
                bridge_name: server_plan.bridge_name.clone(),
                bridge_interface: server_plan.bridge_interface.clone(),
                proxy_address: server_plan.proxy_address,
                dns_address: server_plan.dns_address,
                public_host: proxy::parse_public_host(server_plan)?,
            })
        } else {
            None
        };
        // Written here, not earlier: engine install, network setup, and agent install each
        // already bailed the whole command on any per-host failure before reaching this final
        // phase, so a host that gets this far already succeeded at every earlier step -- this
        // proxy result is the last thing standing between "setup succeeded" and "setup failed"
        // for this host. Kept last deliberately: `ensure_proxy`'s own minimum-version check
        // (`version_requirements::MIN_PROXY_VERSION`) fails on a stale-but-already-running proxy
        // container without recreating it (recreating a host-global, multi-tenant container is
        // `jiji proxy restart`'s job, never an implicit side effect here) -- if this ran before
        // agent install, that failure would block the agent from ever being updated in the same
        // run.
        match proxy::ensure_proxy(&session, config.builder.engine, network, false).await {
            Ok(ProxyStatus::AlreadyRunning) => {
                Ui::say(
                    &format!("{name}: jiji-proxy already configured and running"),
                    1,
                );
                if let Some(handle) = &progress {
                    handle.mark_success(name, "proxy already running");
                }
                audit::record(
                    &session,
                    &config.project,
                    "server_setup",
                    AuditStatus::Success,
                    "engine, network, agent, and jiji-proxy configured",
                    Some(&LockScope::HostRuntime.to_string()),
                    None,
                    Some(started_at.elapsed()),
                )
                .await;
            }
            Ok(ProxyStatus::Started) => {
                Ui::say(&format!("{name}: jiji-proxy configured and running"), 1);
                if let Some(handle) = &progress {
                    handle.mark_success(name, "proxy configured");
                }
                audit::record(
                    &session,
                    &config.project,
                    "server_setup",
                    AuditStatus::Success,
                    "engine, network, agent, and jiji-proxy configured",
                    Some(&LockScope::HostRuntime.to_string()),
                    None,
                    Some(started_at.elapsed()),
                )
                .await;
            }
            Err(error) => {
                Ui::error(&format!("{name}: {error}"));
                if let Some(handle) = &progress {
                    handle.mark_failed(name, &error.to_string());
                }
                audit::record(
                    &session,
                    &config.project,
                    "server_setup",
                    AuditStatus::Failed,
                    format!("jiji-proxy setup failed: {error}"),
                    Some(&LockScope::HostRuntime.to_string()),
                    None,
                    Some(started_at.elapsed()),
                )
                .await;
                failures.push((name.clone(), error.to_string()));
            }
        }
        session.close().await;
    }

    if !failures.is_empty() {
        for (name, error) in &failures {
            Ui::say(&format!("{name}: {error}"), 1);
        }
        anyhow::bail!(
            "Jiji proxy setup failed for {} server(s). Fix the reported hosts and retry `jiji server setup`.",
            failures.len()
        );
    }

    Ok(())
}

/// Filters `servers` down to those whose `host` value matches at least one comma-separated
/// `*`-wildcard pattern. Warns on patterns that match nothing, and fails only if the filter
/// empties the whole set.
fn split_comma_trimmed(value: Option<&str>) -> Vec<String> {
    value
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}
