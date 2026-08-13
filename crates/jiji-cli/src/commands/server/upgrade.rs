//! `jiji server upgrade`: reads each selected host's actual `jiji-agent`/`jiji-proxy` versions,
//! compares them against what this CLI was built against, and upgrades only what's outdated --
//! collapsing the manual `jiji server setup` + `jiji proxy restart` + `jiji network diagnostics`
//! sequence `docs/todo.md` used to document into one command. Reuses `server setup`'s own
//! enrollment machinery (`gather_membership_view`/`enroll_agent_targets`) and `agent_install`/
//! `proxy`'s existing idempotent install primitives; introduces no new agent/proxy RPC.

use std::collections::{BTreeMap, BTreeSet};
use std::io::IsTerminal;
use std::sync::Arc;
use std::time::Instant;

use jiji_agent::api::{RequestBody, ResponseBody};
use jiji_agent::membership::MembershipScope;
use jiji_config::{validate_config, Config, ContainerEngine, NamedServer};
use jiji_network::NetworkPlanner;
use jiji_ssh::{SshPool, SshSession};
use jiji_tui::Ui;

use crate::agent_distribution::{self, AgentBinarySource};
use crate::agent_install;
use crate::audit::{self, AuditStatus};
use crate::commands::server::setup::{
    connect_for_setup, enroll_agent_targets, gather_membership_view,
};
use crate::engine;
use crate::lock::{LockRequest, LockScope};
use crate::proxy::{self, ProxyStatus};
use crate::ssh_adapter;

type ComponentOutcomes = BTreeMap<String, Result<String, String>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VersionStatus {
    Current,
    Outdated,
    Ahead,
    Unknown,
    Unavailable,
}

impl VersionStatus {
    fn label(self) -> &'static str {
        match self {
            VersionStatus::Current => "current",
            VersionStatus::Outdated => "outdated",
            VersionStatus::Ahead => "ahead",
            VersionStatus::Unknown => "unknown",
            VersionStatus::Unavailable => "unavailable",
        }
    }
}

/// `found < required` -> `Outdated`, `found > required` -> `Ahead`, equal -> `Current`; either
/// string failing to parse -> `Unknown` (fails open, same posture as
/// `version_requirements::check_min_version`). Never returns `Unavailable`: that status is decided
/// by the read step itself (connection/command failure), not by this pure comparison.
fn compare_versions(found: &str, required: &str) -> VersionStatus {
    match (
        engine::parse_version(found),
        engine::parse_version(required),
    ) {
        (Some(found), Some(required)) if found < required => VersionStatus::Outdated,
        (Some(found), Some(required)) if found > required => VersionStatus::Ahead,
        (Some(_), Some(_)) => VersionStatus::Current,
        _ => VersionStatus::Unknown,
    }
}

#[derive(Clone)]
struct ComponentRead {
    found: Option<String>,
    status: VersionStatus,
}

fn classify(read: Result<String, String>, required: &str) -> ComponentRead {
    match read {
        Ok(version) => {
            let status = compare_versions(&version, required);
            ComponentRead {
                found: Some(version),
                status,
            }
        }
        Err(_) => ComponentRead {
            found: None,
            status: VersionStatus::Unavailable,
        },
    }
}

struct HostRead {
    agent: ComponentRead,
    proxy: ComponentRead,
}

async fn read_agent_version(session: &SshSession, project: &str) -> Result<String, String> {
    match crate::agent_client::call(session, project, None, RequestBody::Health).await {
        Ok(ResponseBody::Health { version, .. }) => Ok(version),
        Ok(other) => Err(format!("agent returned an unexpected response: {other:?}")),
        Err(error) => Err(error.to_string()),
    }
}

async fn read_proxy_version(
    session: &SshSession,
    engine: ContainerEngine,
) -> Result<String, String> {
    let command = crate::proxy_routes::render_version_command(engine);
    match session.execute(&command).await {
        Ok(result) if result.success => Ok(result.stdout.trim().to_string()),
        Ok(result) => Err(result.stderr.trim().to_string()),
        Err(error) => Err(error.to_string()),
    }
}

fn describe_found(found: &Option<String>) -> String {
    match found {
        Some(version) => format!("v{version}"),
        None => "unavailable".to_string(),
    }
}

pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    hosts: Option<&str>,
    services: Option<&str>,
    yes: bool,
) -> anyhow::Result<()> {
    Ui::section("Server Upgrade:");
    let started_at = Instant::now();

    if services.is_some() {
        anyhow::bail!(
            "`jiji server upgrade` does not accept -S/--services: it upgrades host-level components shared across every service, not per-service state. Use -H/--hosts to select servers instead."
        );
    }

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
            "No `ssh:` section configured in {}. Add at least `ssh.user:` before running server upgrade.",
            path.display()
        )
    })?;

    if config.servers.is_empty() {
        anyhow::bail!("No servers are configured in {}.", path.display());
    }

    let network_plan = NetworkPlanner::new()
        .plan(&config)
        .map_err(|error| anyhow::anyhow!("Could not build the private network plan: {error}"))?;

    let host_filters = split_comma_trimmed(hosts);
    let target_names: BTreeSet<String> = network_plan
        .select_hosts(&host_filters)?
        .into_iter()
        .map(|server| server.name.clone())
        .collect();

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

    Ui::section("Connecting:");
    let pool = SshPool::new(ssh.max_concurrent_starts as usize);
    let mut connect_operations = Vec::with_capacity(servers.len());
    for (name, server) in &servers {
        connect_operations.push(ssh_adapter::connect_options(name, server, &ssh)?);
    }
    let operations: Vec<_> = connect_operations
        .into_iter()
        .map(|options| move || async move { connect_for_setup(&options).await })
        .collect();
    let connections = pool.execute_concurrent(operations).await;

    let mut sessions: BTreeMap<String, Arc<SshSession>> = BTreeMap::new();
    let mut reads: BTreeMap<String, HostRead> = BTreeMap::new();
    for ((name, server), connection) in servers.iter().zip(connections) {
        match connection {
            Ok(session) => {
                Ui::say(&format!("{name} ({}): connected", server.host), 1);
                sessions.insert(name.clone(), Arc::new(session));
            }
            Err(error) => {
                Ui::error(&format!("{name} ({}): {error}", server.host));
                reads.insert(
                    name.clone(),
                    HostRead {
                        agent: ComponentRead {
                            found: None,
                            status: VersionStatus::Unavailable,
                        },
                        proxy: ComponentRead {
                            found: None,
                            status: VersionStatus::Unavailable,
                        },
                    },
                );
            }
        }
    }
    if sessions.is_empty() {
        anyhow::bail!("Could not connect to any server; see the errors above");
    }

    let reachable: Vec<(String, NamedServer)> = servers
        .iter()
        .filter(|(name, _)| sessions.contains_key(name))
        .cloned()
        .collect();

    let lock_requests: Vec<LockRequest> = reachable
        .iter()
        .map(|(name, _)| LockRequest::new(LockScope::HostRuntime, name.clone()))
        .collect();

    Ui::section("Acquiring Locks:");
    let owned_locks = crate::lock::OwnedDeploymentLocks::acquire(
        &pool,
        &sessions,
        &config.project,
        lock_requests,
        format!(
            "jiji server upgrade: {}",
            reachable
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        300,
        false,
    )
    .await?;
    Ui::say(&format!("Acquired {} lock(s).", reachable.len()), 1);

    let upgrade_result: anyhow::Result<(ComponentOutcomes, ComponentOutcomes)> = async {
        Ui::section("Reading Component Versions:");
        for (name, _) in &reachable {
            let session = sessions.get(name).expect("connected above");
            let agent = classify(
                read_agent_version(session, &config.project).await,
                crate::version_requirements::AGENT_BUILD_VERSION,
            );
            let proxy = classify(
                read_proxy_version(session, config.builder.engine).await,
                jiji_network::PROXY_VERSION,
            );
            Ui::say(
                &format!(
                    "{name}: jiji-agent {} ({}), jiji-proxy {} ({})",
                    describe_found(&agent.found),
                    agent.status.label(),
                    describe_found(&proxy.found),
                    proxy.status.label()
                ),
                1,
            );
            reads.insert(name.clone(), HostRead { agent, proxy });
        }

        Ui::section("Plan:");
        let mut any_outdated = false;
        for (name, read) in &reads {
            if matches!(read.agent.status, VersionStatus::Outdated)
                || matches!(read.proxy.status, VersionStatus::Outdated)
            {
                any_outdated = true;
            }
            Ui::say(&format!("{name}:"), 1);
            Ui::say(
                &format!(
                    "jiji-agent: {} -> required v{} [{}]",
                    describe_found(&read.agent.found),
                    crate::version_requirements::AGENT_BUILD_VERSION,
                    plan_action(read.agent.status, true)
                ),
                2,
            );
            Ui::say(
                &format!(
                    "jiji-proxy: {} -> required v{} [{}]",
                    describe_found(&read.proxy.found),
                    jiji_network::PROXY_VERSION,
                    plan_action(read.proxy.status, false)
                ),
                2,
            );
        }

        if !any_outdated {
            Ui::say(
                "Nothing is outdated. Refreshing configuration on already-current hosts.",
                1,
            );
        } else if !yes {
            if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
                anyhow::bail!(
                    "Refusing to prompt for confirmation without a terminal attached. Pass --yes to confirm the upgrade when running non-interactively (e.g. CI/CD)."
                );
            }
            let confirmed = Ui::confirm("Proceed with upgrading these components?", true)?;
            if !confirmed {
                anyhow::bail!("Server upgrade cancelled: not confirmed.");
            }
        }

        let agent_reads: BTreeMap<String, ComponentRead> = reads
            .iter()
            .filter(|(_, read)| !matches!(read.agent.status, VersionStatus::Unavailable))
            .map(|(name, read)| (name.clone(), read.agent.clone()))
            .collect();
        let agent_targets: Vec<(String, NamedServer)> = reachable
            .iter()
            .filter(|(name, _)| agent_reads.contains_key(name))
            .cloned()
            .collect();

        Ui::section("Upgrading Jiji Agent:");
        let host_agent_outcomes = if agent_targets.is_empty() {
            BTreeMap::new()
        } else {
            upgrade_agents(
                &config,
                &path,
                &network_plan,
                &agent_targets,
                &ssh,
                &sessions,
                &agent_reads,
            )
            .await?
        };
        for (name, outcome) in &host_agent_outcomes {
            match outcome {
                Ok(detail) => Ui::result_ok(name, &format!("jiji-agent {detail}")),
                Err(error) => Ui::result_error(name, &format!("jiji-agent: {error}")),
            }
        }

        Ui::section("Upgrading Jiji Proxy:");
        let proxy_status: BTreeMap<String, VersionStatus> = reads
            .iter()
            .filter(|(_, read)| !matches!(read.proxy.status, VersionStatus::Unavailable))
            .map(|(name, read)| (name.clone(), read.proxy.status))
            .collect();
        let proxy_targets: Vec<(String, NamedServer)> = reachable
            .iter()
            .filter(|(name, _)| proxy_status.contains_key(name))
            .cloned()
            .collect();
        let host_proxy_outcomes =
            upgrade_proxies(&config, &proxy_targets, &sessions, &proxy_status).await?;
        for (name, outcome) in &host_proxy_outcomes {
            match outcome {
                Ok(detail) => Ui::result_ok(name, &format!("jiji-proxy {detail}")),
                Err(error) => Ui::result_error(name, &format!("jiji-proxy: {error}")),
            }
        }

        for (name, session) in &sessions {
            let agent_result = host_agent_outcomes.get(name);
            let agent_message = match &reads[name].agent.status {
                VersionStatus::Unavailable => "jiji-agent unavailable".to_string(),
                _ => match agent_result {
                    Some(Ok(detail)) => format!("jiji-agent {detail}"),
                    Some(Err(error)) => format!("jiji-agent failed: {error}"),
                    None => "jiji-agent not attempted".to_string(),
                },
            };
            audit::record(
                session,
                &config.project,
                "server_upgrade",
                if matches!(agent_result, Some(Err(_)))
                    || matches!(reads[name].agent.status, VersionStatus::Unavailable)
                {
                    AuditStatus::Failed
                } else {
                    AuditStatus::Success
                },
                agent_message,
                Some(&LockScope::HostRuntime.to_string()),
                None,
                Some(started_at.elapsed()),
            )
            .await;

            let proxy_result = host_proxy_outcomes.get(name);
            let proxy_message = match &reads[name].proxy.status {
                VersionStatus::Unavailable => "jiji-proxy unavailable".to_string(),
                _ => match proxy_result {
                    Some(Ok(detail)) => format!("jiji-proxy {detail}"),
                    Some(Err(error)) => format!("jiji-proxy failed: {error}"),
                    None => "jiji-proxy not attempted".to_string(),
                },
            };
            audit::record(
                session,
                &config.project,
                "server_upgrade",
                if matches!(proxy_result, Some(Err(_)))
                    || matches!(reads[name].proxy.status, VersionStatus::Unavailable)
                {
                    AuditStatus::Failed
                } else {
                    AuditStatus::Success
                },
                proxy_message,
                Some(&LockScope::HostRuntime.to_string()),
                None,
                Some(started_at.elapsed()),
            )
            .await;
        }

        Ok((host_agent_outcomes, host_proxy_outcomes))
    }
    .await;

    Ui::section("Releasing Locks:");
    let release_result = owned_locks.release(&pool, &sessions).await;
    for session in sessions.values() {
        session.close().await;
    }
    let (agent_outcomes, proxy_outcomes) = match (upgrade_result, release_result) {
        (Ok(outcomes), Ok(())) => outcomes,
        (Err(error), Ok(())) => return Err(error),
        (Ok(_), Err(release_error)) => return Err(release_error),
        (Err(error), Err(release_error)) => {
            return Err(error.context(format!("Additionally, {release_error}")))
        }
    };

    Ui::section("Summary:");
    let mut failed = false;
    for (name, read) in &reads {
        if matches!(read.agent.status, VersionStatus::Unavailable) {
            failed = true;
        }
        if matches!(read.proxy.status, VersionStatus::Unavailable) {
            failed = true;
        }
        if matches!(agent_outcomes.get(name), Some(Err(_))) {
            failed = true;
        }
        if matches!(proxy_outcomes.get(name), Some(Err(_))) {
            failed = true;
        }
        Ui::say(&format!("{name}:"), 1);
        Ui::say(
            &format!(
                "jiji-agent: {} (required v{}) -> {}",
                describe_found(&read.agent.found),
                crate::version_requirements::AGENT_BUILD_VERSION,
                summary_result(read.agent.status, agent_outcomes.get(name))
            ),
            2,
        );
        Ui::say(
            &format!(
                "jiji-proxy: {} (required v{}) -> {}",
                describe_found(&read.proxy.found),
                jiji_network::PROXY_VERSION,
                summary_result(read.proxy.status, proxy_outcomes.get(name))
            ),
            2,
        );
    }

    Ui::section("Diagnostics:");
    if let Err(error) =
        crate::commands::network::diagnostics::run(environment, config_file, hosts, false).await
    {
        let mut fallback = "jiji network diagnostics".to_string();
        if let Some(environment) = environment {
            fallback.push_str(&format!(" -e {environment}"));
        }
        if let Some(hosts) = hosts {
            fallback.push_str(&format!(" -H {hosts}"));
        }
        Ui::warn(&format!(
            "Could not run diagnostics automatically: {error}. Run `{fallback}` to check host health."
        ));
    }

    Ui::say(
        "Run 'jiji server upgrade' again for each other environment configuration (-e staging, -e production, ...).",
        1,
    );

    if failed {
        anyhow::bail!(
            "Server upgrade did not complete cleanly for every host; see the summary above."
        );
    }

    Ui::success_elapsed("Server upgrade complete.", started_at.elapsed());
    Ok(())
}

/// The final summary's per-component result: the same detail string the live apply-phase row
/// already printed, so the summary reflects the actual outcome rather than silently repeating the
/// pre-upgrade read under a "Summary" heading (a component that was upgraded shows "upgraded ..."
/// here, not the stale previous version this row's `describe_found` also displays).
fn summary_result(status: VersionStatus, outcome: Option<&Result<String, String>>) -> String {
    if matches!(status, VersionStatus::Unavailable) {
        return "unavailable".to_string();
    }
    match outcome {
        Some(Ok(detail)) => detail.clone(),
        Some(Err(error)) => format!("failed: {error}"),
        None => "not attempted".to_string(),
    }
}

fn plan_action(status: VersionStatus, is_agent: bool) -> &'static str {
    match (status, is_agent) {
        (VersionStatus::Outdated, _) => "upgrade",
        (VersionStatus::Current, _) => "refresh config",
        (VersionStatus::Unknown, _) => "refresh config (unparseable version)",
        (VersionStatus::Ahead, true) => "refresh config (ahead of required, binary not touched)",
        (VersionStatus::Ahead, false) => "skip (ahead of required, would downgrade)",
        (VersionStatus::Unavailable, _) => "skip (unavailable)",
    }
}

/// Applies agent version-comparison results for `targets` (already filtered to reachable,
/// non-`Unavailable`-agent hosts): resolves a binary source the same way `server setup` does,
/// enrolls every target through the shared `gather_membership_view`/`enroll_agent_targets`
/// machinery, then calls `agent_install::ensure_agent` per host -- passing a real binary path only
/// for `Current`/`Outdated` (an `Ahead`/`Unknown` host always gets `binary_path: None`, refreshing
/// config/unit/membership without ever touching its binary).
async fn upgrade_agents(
    config: &Config,
    config_path: &std::path::Path,
    network_plan: &jiji_network::NetworkPlan,
    targets: &[(String, NamedServer)],
    ssh: &jiji_config::Ssh,
    lock_sessions: &BTreeMap<String, Arc<SshSession>>,
    agent_reads: &BTreeMap<String, ComponentRead>,
) -> anyhow::Result<ComponentOutcomes> {
    let binary_source = match agent_install::find_local_agent_binary() {
        agent_install::LocalAgentBinary::Found(path) => AgentBinarySource::Local(path),
        agent_install::LocalAgentBinary::ExplicitOverrideInvalid(message) => {
            anyhow::bail!("jiji server upgrade requires the authoritative jiji agent: {message}");
        }
        agent_install::LocalAgentBinary::NotConfigured => {
            let download = agent_distribution::managed_download_config();
            Ui::say(
                &format!(
                    "No local jiji-agent binary found; installing jiji-agent v{} from the \
                     release on hosts that need it.",
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

    let target_names: BTreeSet<String> = targets.iter().map(|(name, _)| name.clone()).collect();
    let recovery_epoch = crate::recovery_epoch::read(config_path)?;
    let scope = MembershipScope::new(config.project.clone(), recovery_epoch);
    let view = gather_membership_view(config, ssh, lock_sessions, &target_names, &scope).await;
    let (prepared, records, enroll_failures) = enroll_agent_targets(
        config,
        network_plan,
        targets,
        ssh,
        recovery_epoch,
        &scope,
        view,
    )
    .await?;

    let mut outcomes: ComponentOutcomes = BTreeMap::new();
    for (name, error) in enroll_failures {
        outcomes.insert(name, Err(error));
    }

    for (name, session, mesh_config) in prepared {
        let read = agent_reads.get(&name);
        let status = read
            .map(|read| read.status)
            .unwrap_or(VersionStatus::Unknown);
        let touch_binary = matches!(status, VersionStatus::Current | VersionStatus::Outdated);
        let binary_path = if touch_binary {
            match &binary_source {
                AgentBinarySource::Local(path) => Some(path.clone()),
                AgentBinarySource::Managed(_) => {
                    let script = remote_install_script
                        .as_ref()
                        .expect("remote script is rendered whenever managed mode is used");
                    match session.execute(script).await {
                        Ok(result) if result.success => None,
                        Ok(result) => {
                            outcomes.insert(
                                name.clone(),
                                Err(format!(
                                    "could not download the jiji agent from the release: {}",
                                    result.stderr.trim()
                                )),
                            );
                            session.close().await;
                            continue;
                        }
                        Err(error) => {
                            outcomes.insert(name.clone(), Err(error.to_string()));
                            session.close().await;
                            continue;
                        }
                    }
                }
            }
        } else {
            None
        };

        let result = agent_install::ensure_agent(
            &session,
            config.builder.engine,
            &config.project,
            binary_path.as_deref(),
            &mesh_config,
            &records,
        )
        .await;
        match result {
            Ok(agent_result) => {
                if let Err(error) = crate::commands::network::membership::push_membership(
                    &session,
                    &config.project,
                    &records,
                )
                .await
                {
                    outcomes.insert(name.clone(), Err(error.to_string()));
                } else {
                    let detail = match (status, agent_result) {
                        (VersionStatus::Outdated, agent_install::AgentStatus::Upgraded) => {
                            format!(
                                "upgraded {} -> v{}",
                                describe_found(&read.and_then(|read| read.found.clone())),
                                crate::version_requirements::AGENT_BUILD_VERSION
                            )
                        }
                        (VersionStatus::Ahead, _) => {
                            "is ahead of required version, config refreshed (binary not touched)"
                                .to_string()
                        }
                        (VersionStatus::Unknown, _) => {
                            "has an unparseable version, config refreshed (binary not touched)"
                                .to_string()
                        }
                        (_, agent_install::AgentStatus::Installed) => "installed".to_string(),
                        (_, agent_install::AgentStatus::Upgraded) => "binary upgraded".to_string(),
                        (_, agent_install::AgentStatus::AlreadyRunning) => {
                            "already current, config refreshed".to_string()
                        }
                    };
                    outcomes.insert(name.clone(), Ok(detail));
                }
            }
            Err(error) => {
                outcomes.insert(name.clone(), Err(error.to_string()));
            }
        }
        session.close().await;
    }

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

    Ok(outcomes)
}

/// Applies proxy version-comparison results for `targets` (already filtered to reachable,
/// non-`Unavailable`-proxy hosts): `Outdated` forces a recreate; `Current`/`Unknown` only refresh
/// daemon config (`ensure_proxy`'s own `is_current_and_running` check decides whether a recreate is
/// independently still needed, expected to be a no-op here); `Ahead` is skipped entirely -- no
/// config refresh either, since recreating is the only path that re-applies daemon config today,
/// and this command must never recreate an ahead-of-required proxy.
async fn upgrade_proxies(
    config: &Config,
    targets: &[(String, NamedServer)],
    lock_sessions: &BTreeMap<String, Arc<SshSession>>,
    proxy_status: &BTreeMap<String, VersionStatus>,
) -> anyhow::Result<ComponentOutcomes> {
    let plan = NetworkPlanner::new()
        .plan(config)
        .map_err(|error| anyhow::anyhow!("Could not build the proxy network plan: {error}"))?;
    let dns_enabled = plan.enabled;

    let mut outcomes = BTreeMap::new();
    for (name, _) in targets {
        let status = proxy_status
            .get(name)
            .copied()
            .unwrap_or(VersionStatus::Unknown);
        if matches!(status, VersionStatus::Ahead) {
            outcomes.insert(
                name.clone(),
                Ok("is ahead of required version, not upgraded".to_string()),
            );
            continue;
        }
        let session = lock_sessions
            .get(name)
            .expect("locked session exists for a targeted host");
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
        let force = matches!(status, VersionStatus::Outdated);
        match proxy::ensure_proxy(session, config.builder.engine, network, force).await {
            Ok(ProxyStatus::AlreadyRunning) => {
                outcomes.insert(name.clone(), Ok("already current".to_string()));
            }
            Ok(ProxyStatus::Started) => {
                outcomes.insert(
                    name.clone(),
                    Ok(if force {
                        format!("upgraded, now v{}", jiji_network::PROXY_VERSION)
                    } else {
                        "started".to_string()
                    }),
                );
            }
            Err(error) => {
                outcomes.insert(name.clone(), Err(error.to_string()));
            }
        }
    }
    Ok(outcomes)
}

/// Filters `servers` down to those whose `host` value matches at least one comma-separated
/// `*`-wildcard pattern, matching `server setup`/`server teardown`'s own precedent.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_older_found_version_is_outdated() {
        assert_eq!(compare_versions("0.4.9", "0.6.4"), VersionStatus::Outdated);
    }

    #[test]
    fn an_equal_found_version_is_current() {
        assert_eq!(compare_versions("0.6.4", "0.6.4"), VersionStatus::Current);
    }

    #[test]
    fn a_newer_found_version_is_ahead() {
        assert_eq!(compare_versions("0.7.0", "0.6.4"), VersionStatus::Ahead);
    }

    #[test]
    fn an_unparseable_found_version_is_unknown() {
        assert_eq!(
            compare_versions("not-a-version", "0.6.4"),
            VersionStatus::Unknown
        );
    }

    #[test]
    fn an_unparseable_required_version_is_unknown() {
        assert_eq!(
            compare_versions("0.6.4", "also-not-a-version"),
            VersionStatus::Unknown
        );
    }

    #[test]
    fn classify_maps_a_read_error_to_unavailable() {
        let read = classify(Err("connection refused".to_string()), "0.6.4");
        assert_eq!(read.status, VersionStatus::Unavailable);
        assert_eq!(read.found, None);
    }

    #[test]
    fn classify_maps_a_successful_read_through_compare_versions() {
        let read = classify(Ok("0.6.4".to_string()), "0.6.4");
        assert_eq!(read.status, VersionStatus::Current);
        assert_eq!(read.found, Some("0.6.4".to_string()));
    }
}
