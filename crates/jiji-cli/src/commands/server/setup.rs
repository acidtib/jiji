use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::sync::Arc;

use jiji_agent::membership::{
    MembershipRecord, MembershipState, SignedMembership, MEMBERSHIP_PROTOCOL_VERSION,
    MEMBERSHIP_SCHEMA_VERSION,
};
use jiji_agent::runtime::{AuthorityConfig, MeshConfig};
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

pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    hosts: Option<&str>,
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
        .map(|options| move || async move { SshSession::connect(&options).await })
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

    let setup_result =
        crate::commands::lock::with_locks(
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
            || async {
                let pool = SshPool::new(ssh.max_concurrent_starts as usize);
                let mut connect_operations = Vec::with_capacity(servers.len());
                for (name, server) in &servers {
                    connect_operations.push(ssh_adapter::connect_options(name, server, &ssh)?);
                }

                Ui::section("Connecting:");
                let operations: Vec<_> = connect_operations
                    .into_iter()
                    .map(|options| move || async move { SshSession::connect(&options).await })
                    .collect();
                let connections = pool.execute_concurrent(operations).await;

                let mut failures: Vec<(String, String)> = Vec::new();
                let mut sessions: Vec<(String, SshSession)> = Vec::new();
                for ((name, server), connection) in servers.iter().zip(connections) {
                    match connection {
                        Ok(session) => {
                            Ui::say(&format!("{name} ({}): connected", server.host), 1);
                            sessions.push((name.clone(), session));
                        }
                        Err(err) => {
                            Ui::error(&format!("{name} ({}): {err}", server.host));
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
                    match engine::ensure_engine(session, engine).await {
                        Ok(EngineStatus::AlreadyInstalled(version)) => {
                            Ui::say(&format!("{engine} already installed ({version})"), 2);
                        }
                        Ok(EngineStatus::Installed(version)) => {
                            Ui::say(&format!("{engine} installed ({version})"), 2);
                        }
                        Ok(EngineStatus::Upgraded { from, to }) => {
                            Ui::say(&format!("{engine} upgraded ({from} -> {to})"), 2);
                        }
                        Err(err) => {
                            Ui::error(&format!("  {err}"));
                            failures.push((name.clone(), err.to_string()));
                        }
                    }
                    session.close().await;
                }

                if !failures.is_empty() {
                    Ui::error(&format!("\n{} server(s) failed:", failures.len()));
                    for (name, message) in &failures {
                        Ui::say(&format!("{name}: {message}"), 1);
                    }
                    anyhow::bail!("Server setup failed for {} server(s)", failures.len());
                }

                network::setup::reconcile_for_server_setup(&config, &network_plan, &target_names)
                    .await
                    .map_err(|error| {
                        anyhow::anyhow!(
                "Container engine setup succeeded, but complete network setup failed: {error}"
            )
                    })?;

                setup_proxies(&config, &servers, &ssh, started_at).await?;

                setup_agents(&config, &path, &network_plan, &servers, &ssh).await?;
                let replayed = crate::commands::network::backup::replay_recovery_desired_state(
                    &path, &config, &servers, &ssh,
                )
                .await?;
                if replayed > 0 {
                    Ui::result_ok(
            "recovery",
            &format!("restored desired placement for {replayed} service(s) into the new epoch"),
        );
                }

                Ok(())
            },
        )
        .await;
    close_all(&lock_sessions).await;
    setup_result?;

    Ui::success_elapsed("All servers are ready.", started_at.elapsed());
    Ok(())
}

async fn close_all(sessions: &BTreeMap<String, Arc<SshSession>>) {
    for session in sessions.values() {
        session.close().await;
    }
}

async fn setup_agents(
    config: &Config,
    config_path: &std::path::Path,
    network_plan: &NetworkPlan,
    servers: &[(String, NamedServer)],
    ssh: &Ssh,
) -> anyhow::Result<()> {
    // Spike: the agent no longer has to sit next to the CLI. Local discovery (env override,
    // sibling binary) still wins; otherwise fall back to a host-side install script that
    // downloads the matching-version agent from the GitHub release, verified by sha256, on
    // each remote host being set up.
    let binary_source = match agent_install::find_local_agent_binary() {
        agent_install::LocalAgentBinary::Found(path) => AgentBinarySource::Local(path),
        agent_install::LocalAgentBinary::ExplicitOverrideInvalid(message) => {
            anyhow::bail!("Phase 3 requires the authoritative jiji agent: {message}");
        }
        agent_install::LocalAgentBinary::NotConfigured => {
            let download = agent_distribution::managed_download_config();
            Ui::say(
                &format!(
                    "No local jiji-agent binary found; installing jiji-agent v{} from the \
                     release on each server.",
                    download.version
                ),
                1,
            );
            AgentBinarySource::Managed(download)
        }
    };
    let remote_install_script = match &binary_source {
        AgentBinarySource::Managed(download) => {
            let paths = jiji_agent::AgentPaths::default_for_project(&config.project);
            let bin_dir = paths
                .binary_path
                .parent()
                .expect("binary path always has a parent directory");
            Some(agent_distribution::remote_install_script(
                &download.base_url,
                &download.version,
                &paths.project_dir,
                bin_dir,
                &paths.state_dir,
                &paths.binary_path,
            ))
        }
        AgentBinarySource::Local(_) => None,
    };
    let authority = crate::membership_authority::ProjectAuthority::load_or_create(config_path)?;

    Ui::section("Installing Jiji Agent:");
    let pool = SshPool::new(ssh.max_concurrent_starts as usize);
    let mut connect_operations = Vec::with_capacity(servers.len());
    for (name, server) in servers {
        connect_operations.push(ssh_adapter::connect_options(name, server, ssh)?);
    }
    let operations: Vec<_> = connect_operations
        .into_iter()
        .map(|options| move || async move { SshSession::connect(&options).await })
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
        let node_signing_key =
            crate::membership_authority::ProjectAuthority::load_or_create_node_key(
                config_path,
                name,
            )?;
        let endpoint: SocketAddr =
            format!("{}:{}", server_plan.public_host, server_plan.wireguard_port)
                .parse()
                .map_err(|_| {
                    anyhow::anyhow!(
                "Server '{}' host '{}' must be a public IP address for signed mesh enrollment",
                name,
                server_plan.public_host
            )
                })?;
        let record = MembershipRecord {
            project_id: config.project.clone(),
            recovery_epoch: crate::membership_authority::recovery_epoch(config_path)?,
            protocol_version: MEMBERSHIP_PROTOCOL_VERSION,
            schema_version: MEMBERSHIP_SCHEMA_VERSION,
            node_id: name.clone(),
            server_name: name.clone(),
            node_signing_public_key: node_signing_key.verifying_key().to_bytes().to_vec(),
            wireguard_public_key: key_result.stdout.trim().to_string(),
            management_address: server_plan.management_address,
            container_subnet: server_plan.container_subnet.to_string(),
            endpoints: vec![endpoint],
            owner_epoch: 1,
            revision: 1,
            state: MembershipState::Active,
        };
        let operation = SignedMembership::sign(record, &authority.id, &authority.signing_key)?;
        let peer_public_ips = network::bridge::BridgeProvisioner::new(
            config.builder.engine,
            network_plan,
            server_plan,
        )
        .peer_public_ips()?;
        let membership_port = jiji_network::membership_replication_port(&config.project);
        let replication_peers = network_plan
            .servers
            .iter()
            .filter(|(peer_name, _)| *peer_name != name)
            .map(|(_, peer)| SocketAddr::new(peer.management_address.into(), membership_port))
            .collect();
        let mesh_config = MeshConfig {
            project_id: config.project.clone(),
            recovery_epoch: crate::membership_authority::recovery_epoch(config_path)?,
            node_id: name.clone(),
            node_signing_key: node_signing_key.to_bytes().to_vec(),
            wireguard_interface: server_plan.wireguard_interface.clone(),
            wireguard_private_key_path: network::setup::private_key_path(&slug).into(),
            replication_bind: SocketAddr::new(
                server_plan.management_address.into(),
                membership_port,
            ),
            dns_bind_address: server_plan.dns_address,
            local_runtime: jiji_agent::runtime::LocalRuntimeConfig {
                bridge_network: server_plan.bridge_name.clone(),
                bridge_interface: server_plan.bridge_interface.clone(),
                proxy_address: server_plan.proxy_address,
                proxy_routes: config
                    .services
                    .iter()
                    .filter(|(_, service)| service.servers.contains(name))
                    .filter_map(|(service_name, service)| {
                        service.proxy.as_ref().map(|proxy| (service_name, proxy))
                    })
                    .flat_map(|(service_name, proxy)| {
                        crate::proxy_routes::runtime_specs_for_service(
                            config.builder.engine,
                            &config.project,
                            service_name,
                            proxy,
                        )
                    })
                    .collect(),
                container_subnet: server_plan.container_subnet,
                bridge_gateway: server_plan.bridge_gateway,
                container_cidr: network_plan.container_cidr,
                wireguard_port: server_plan.wireguard_port,
                peer_public_ips,
                public_host: server_plan.public_host.clone(),
            },
            replication_peers,
            authorities: vec![AuthorityConfig {
                id: authority.id.clone(),
                public_key: authority.signing_key.verifying_key().to_bytes().to_vec(),
            }],
            reconcile_interval_secs: 10,
            store_soft_quota_bytes: jiji_agent::runtime::DEFAULT_STORE_SOFT_QUOTA_BYTES,
            compaction_interval_secs: jiji_agent::runtime::DEFAULT_COMPACTION_INTERVAL_SECS,
            dns_forwarders: config
                .network
                .as_ref()
                .map(|network| network.dns_forwarders.clone())
                .unwrap_or_else(jiji_config::default_dns_forwarders),
        };
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
        match agent_install::ensure_agent(
            &session,
            config.builder.engine,
            &config.project,
            binary_path.as_deref(),
            &mesh_config,
            &[operation],
        )
        .await
        {
            Ok(agent_install::AgentStatus::AlreadyRunning) => {
                Ui::say(&format!("{name}: jiji agent already running"), 1);
            }
            Ok(agent_install::AgentStatus::Installed) => {
                Ui::say(&format!("{name}: jiji agent installed and running"), 1);
            }
            Ok(agent_install::AgentStatus::Upgraded) => {
                Ui::say(&format!("{name}: jiji agent binary upgraded"), 1);
            }
            Err(error) => {
                Ui::error(&format!("{name}: {error}"));
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
            "Jiji agent install failed for {} server(s). Fix the reported hosts and retry `jiji server setup`.",
            failures.len()
        );
    }
    Ok(())
}

async fn setup_proxies(
    config: &Config,
    servers: &[(String, NamedServer)],
    ssh: &Ssh,
    started_at: std::time::Instant,
) -> anyhow::Result<()> {
    let plan = NetworkPlanner::new()
        .plan(config)
        .map_err(|error| anyhow::anyhow!("Could not build the proxy network plan: {error}"))?;
    let dns_enabled = plan.enabled;

    Ui::section("Configuring Kamal Proxy:");
    let pool = SshPool::new(ssh.max_concurrent_starts as usize);
    let mut connect_operations = Vec::with_capacity(servers.len());
    for (name, server) in servers {
        connect_operations.push(ssh_adapter::connect_options(name, server, ssh)?);
    }
    let operations: Vec<_> = connect_operations
        .into_iter()
        .map(|options| move || async move { SshSession::connect(&options).await })
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
        // Written here, not earlier: engine install and network setup each already bailed the
        // whole command on any per-host failure before reaching this final phase, so a host that
        // gets this far already succeeded at every earlier step -- this proxy result is the last
        // thing standing between "setup succeeded" and "setup failed" for this host.
        match proxy::ensure_proxy(&session, config.builder.engine, network, false).await {
            Ok(ProxyStatus::AlreadyRunning) => {
                Ui::say(
                    &format!("{name}: kamal-proxy already configured and running"),
                    1,
                );
                audit::record(
                    &session,
                    &config.project,
                    "server_setup",
                    AuditStatus::Success,
                    "engine, network, and kamal-proxy configured",
                    Some(&LockScope::HostRuntime.to_string()),
                    None,
                    Some(started_at.elapsed()),
                )
                .await;
            }
            Ok(ProxyStatus::Started) => {
                Ui::say(&format!("{name}: kamal-proxy configured and running"), 1);
                audit::record(
                    &session,
                    &config.project,
                    "server_setup",
                    AuditStatus::Success,
                    "engine, network, and kamal-proxy configured",
                    Some(&LockScope::HostRuntime.to_string()),
                    None,
                    Some(started_at.elapsed()),
                )
                .await;
            }
            Err(error) => {
                Ui::error(&format!("{name}: {error}"));
                audit::record(
                    &session,
                    &config.project,
                    "server_setup",
                    AuditStatus::Failed,
                    format!("kamal-proxy setup failed: {error}"),
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
            "Kamal proxy setup failed for {} server(s). Fix the reported hosts and retry `jiji server setup`.",
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
