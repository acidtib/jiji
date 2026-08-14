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
use crate::commands::deploy::split_comma_trimmed;
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

    let upgrade_result: anyhow::Result<(ComponentOutcomes, ComponentOutcomes, Vec<String>)> = async {
        Ui::section("Reading Component Versions:");
        // Fan the per-host reads out over the session pool rather than awaiting each host's
        // two SSH round-trips in sequence: every session is already open and independent, so
        // this is pure latency saved, and the pool's own concurrency cap keeps the fan-out
        // under `max_concurrent_starts` for rate-limited SSH firewalls.
        let read_operations: Vec<_> = reachable
            .iter()
            .map(|(name, _)| {
                let session = Arc::clone(sessions.get(name).expect("connected above"));
                let project = config.project.clone();
                let engine = config.builder.engine;
                let name = name.clone();
                move || {
                    let session = Arc::clone(&session);
                    let project = project.clone();
                    let name = name.clone();
                    async move {
                        let agent = classify(
                            read_agent_version(&session, &project).await,
                            crate::version_requirements::AGENT_BUILD_VERSION,
                        );
                        let proxy = classify(
                            read_proxy_version(&session, engine).await,
                            jiji_network::PROXY_VERSION,
                        );
                        (name, agent, proxy)
                    }
                }
            })
            .collect();
        for (name, agent, proxy) in pool.execute_concurrent(read_operations).await {
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
            reads.insert(name, HostRead { agent, proxy });
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

        // `Ahead` is excluded here, not just inside `upgrade_agents`: it must never be touched at
        // all (`docs`'s "ahead of required -> never touched, reported as such"), and
        // `ensure_agent` unconditionally stops the unit / rewrites its config / re-imports
        // membership regardless of `binary_path`, so the only way to honor that is to keep an
        // ahead-of-required host out of `upgrade_agents`'s target list entirely.
        let agent_reads: BTreeMap<String, ComponentRead> = reads
            .iter()
            .filter(|(_, read)| {
                !matches!(
                    read.agent.status,
                    VersionStatus::Unavailable | VersionStatus::Ahead
                )
            })
            .map(|(name, read)| (name.clone(), read.agent.clone()))
            .collect();
        let agent_targets: Vec<(String, NamedServer)> = reachable
            .iter()
            .filter(|(name, _)| agent_reads.contains_key(name))
            .cloned()
            .collect();
        let ahead_names: BTreeSet<String> = reads
            .iter()
            .filter(|(_, read)| matches!(read.agent.status, VersionStatus::Ahead))
            .map(|(name, _)| name.clone())
            .collect();

        Ui::section("Upgrading Jiji Agent:");
        let (mut host_agent_outcomes, membership_problems) = if agent_targets.is_empty() {
            (BTreeMap::new(), Vec::new())
        } else {
            upgrade_agents(
                &config,
                &path,
                &network_plan,
                &agent_targets,
                &ssh,
                &sessions,
                &agent_reads,
                &ahead_names,
            )
            .await?
        };
        for (name, read) in reads
            .iter()
            .filter(|(_, read)| matches!(read.agent.status, VersionStatus::Ahead))
        {
            // `entry`/`or_insert_with`, not a plain `insert`: `upgrade_agents` may already have
            // recorded a real outcome for this host (e.g. a membership-push failure through its
            // still-open lock session) that this friendly "not touched" default must never
            // silently overwrite.
            host_agent_outcomes.entry(name.clone()).or_insert_with(|| {
                Ok(format!(
                    "is ahead of required v{} (found {}); not touched",
                    crate::version_requirements::AGENT_BUILD_VERSION,
                    describe_found(&read.agent.found)
                ))
            });
        }
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
            audit_component(
                session,
                &config.project,
                started_at,
                "jiji-agent",
                reads[name].agent.status,
                host_agent_outcomes.get(name),
            )
            .await;
            audit_component(
                session,
                &config.project,
                started_at,
                "jiji-proxy",
                reads[name].proxy.status,
                host_proxy_outcomes.get(name),
            )
            .await;
        }

        Ok((host_agent_outcomes, host_proxy_outcomes, membership_problems))
    }
    .await;

    Ui::section("Releasing Locks:");
    let release_result = owned_locks.release(&pool, &sessions).await;
    for session in sessions.values() {
        session.close().await;
    }
    let (agent_outcomes, proxy_outcomes, membership_problems) =
        match (upgrade_result, release_result) {
            (Ok(outcomes), Ok(())) => outcomes,
            (Err(error), Ok(())) => return Err(error),
            (Ok(_), Err(release_error)) => return Err(release_error),
            (Err(error), Err(release_error)) => {
                return Err(error.context(format!("Additionally, {release_error}")))
            }
        };

    Ui::section("Summary:");
    // A mesh-wide membership push failure isn't tied to any one host's agent/proxy status, so it
    // can't be folded into the per-host loop below -- but it still means the mesh is left
    // desynchronized, so it must still fail the command the same way `jiji server setup` fails on
    // the identical underlying failure, instead of only ever warning about it.
    let mut failed = !membership_problems.is_empty();
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
/// One `server_upgrade` audit entry per component: the message folds the version read and the
/// upgrade outcome into the same shape for both jiji-agent and jiji-proxy.
async fn audit_component(
    session: &SshSession,
    project: &str,
    started_at: std::time::Instant,
    component: &str,
    status: VersionStatus,
    outcome: Option<&Result<String, String>>,
) {
    let message = match status {
        VersionStatus::Unavailable => format!("{component} unavailable"),
        _ => match outcome {
            Some(Ok(detail)) => format!("{component} {detail}"),
            Some(Err(error)) => format!("{component} failed: {error}"),
            None => format!("{component} not attempted"),
        },
    };
    audit::record(
        session,
        project,
        "server_upgrade",
        if matches!(outcome, Some(Err(_))) || matches!(status, VersionStatus::Unavailable) {
            AuditStatus::Failed
        } else {
            AuditStatus::Success
        },
        message,
        Some(&LockScope::HostRuntime.to_string()),
        None,
        Some(started_at.elapsed()),
    )
    .await;
}

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
#[allow(clippy::too_many_arguments)]
async fn upgrade_agents(
    config: &Config,
    config_path: &std::path::Path,
    network_plan: &jiji_network::NetworkPlan,
    targets: &[(String, NamedServer)],
    ssh: &jiji_config::Ssh,
    lock_sessions: &BTreeMap<String, Arc<SshSession>>,
    agent_reads: &BTreeMap<String, ComponentRead>,
    ahead_names: &BTreeSet<String>,
) -> anyhow::Result<(ComponentOutcomes, Vec<String>)> {
    let (binary_source, remote_install_script) = agent_distribution::resolve_agent_binary_source(
        &config.project,
        "jiji server upgrade requires the authoritative jiji agent",
        |version| {
            format!(
                "No local jiji-agent binary found; installing jiji-agent v{version} from the \
                 release on hosts that need it."
            )
        },
    )
    .await?;

    // The full set of hosts this run already holds a `HostRuntime` lock (and an open session)
    // on -- not just `targets` (which excludes `Ahead`/agent-`Unavailable` hosts): using the
    // narrower set here made `gather_membership_view`'s fast path skip an already-connected
    // host's own session and fall through to a second, redundant connection via
    // `gather_membership`'s mesh-wide fallback below, exactly what the SSH Connection
    // Management docs warn can trip `ufw limit ssh` on repeated runs.
    let target_names: BTreeSet<String> = lock_sessions.keys().cloned().collect();
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

    // Tracks every host reached below through a session opened specifically for enrollment
    // (`prepared`'s own connection, separate from `lock_sessions`) so the follow-up loop after
    // it never pushes a second time through `lock_sessions`' own still-open session for the same
    // host.
    let mut directly_reached: BTreeSet<String> = BTreeSet::new();

    for (name, session, mesh_config) in prepared {
        directly_reached.insert(name.clone());
        let read = agent_reads.get(&name);
        // `Ahead` never reaches this loop at all: the caller (`run`) excludes it from `targets`
        // entirely, matching the documented "ahead of required -> never touched" contract --
        // `ensure_agent` unconditionally stops the unit, rewrites its config, and re-imports
        // membership regardless of `binary_path`, so the only way to genuinely never touch an
        // ahead-of-required host is to never call it for one.
        let status = read
            .map(|read| read.status)
            .unwrap_or(VersionStatus::Unknown);
        // Only an `Outdated` host actually needs a binary change. `Local` source's re-upload for
        // an already-`Current` host is a cheap local hash compare (no network dependency), so it
        // stays harmless to keep; `Managed` source's equivalent is a remote script whose *first*
        // action is an unconditional `curl` of the `.sha256` sidecar before it even checks
        // whether the installed binary already matches -- running it for a `Current` host makes
        // an otherwise no-op refresh depend on GitHub being reachable for no reason, since this
        // CLI already knows the host is current from the `Health` RPC read that classified it.
        let mut managed_binary_changed = false;
        let binary_path = match (&binary_source, status) {
            (AgentBinarySource::Local(path), VersionStatus::Current | VersionStatus::Outdated) => {
                Some(path.clone())
            }
            (AgentBinarySource::Managed(_), VersionStatus::Outdated) => {
                let script = remote_install_script
                    .as_ref()
                    .expect("remote script is rendered whenever managed mode is used");
                match session.execute(script).await {
                    Ok(result) if result.success => {
                        managed_binary_changed =
                            agent_distribution::remote_install_script_changed_binary(
                                &result.stdout,
                            );
                        None
                    }
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
            _ => None,
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
                    // `ensure_agent` is always called with `binary_path: None` in managed mode
                    // (the script above already did any real install work), so its own
                    // `AgentStatus` can't distinguish "the script just installed a new binary"
                    // from "nothing needed to change" -- both come back as `AlreadyRunning`.
                    // `managed_binary_changed` (from the script's own trailing marker) is the
                    // only place that distinction still exists; without it, an outdated host
                    // upgraded through managed mode was reported as merely "already current".
                    let upgraded_detail = format!(
                        "upgraded {} -> v{}",
                        describe_found(&read.and_then(|read| read.found.clone())),
                        crate::version_requirements::AGENT_BUILD_VERSION
                    );
                    let detail = match (status, agent_result) {
                        (VersionStatus::Outdated, agent_install::AgentStatus::Upgraded) => {
                            upgraded_detail
                        }
                        // Managed mode always reports `AlreadyRunning` (see above); the
                        // script's own marker is the only evidence the binary was replaced.
                        (VersionStatus::Outdated, _) if managed_binary_changed => upgraded_detail,
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

    // Every other host this run holds a `HostRuntime` lock on -- agent-unreachable via `Health`
    // (never in `targets` at all), or one that failed partway through `enroll_agent_targets`
    // (that attempt's own separate connection is already closed, but its `lock_sessions` entry
    // never was) -- still has an already-open session right here. Push membership directly
    // through it instead of leaving it to `push_membership_everywhere`'s mesh-wide fallback
    // below: that fallback would otherwise either open a second, redundant connection to a host
    // already connected this run, or (now that `target_names` covers every locked host, see
    // above) never reach it at all. An enroll failure's outcome is left as the more specific
    // error already recorded above; every other host gets its push result recorded fresh.
    //
    // `Ahead` is skipped here, same as everywhere else in this function: the documented
    // "ahead of required -> never touched" contract covers membership too, so an ahead host's
    // still-open session is used above only to *read* its membership (harmless), never to push
    // to it -- it stays excluded from `push_membership_everywhere` below via `target_names`
    // (which is `lock_sessions.keys()`, not filtered to `targets`) too, so it gets no push from
    // either path.
    for (name, session) in lock_sessions {
        if directly_reached.contains(name) || ahead_names.contains(name) {
            continue;
        }
        match crate::commands::network::membership::push_membership(
            session,
            &config.project,
            &records,
        )
        .await
        {
            Ok(()) => {
                outcomes
                    .entry(name.clone())
                    .or_insert_with(|| Ok("membership refreshed".to_string()));
            }
            Err(error) => {
                outcomes
                    .entry(name.clone())
                    .or_insert_with(|| Err(error.to_string()));
            }
        }
    }

    // Never `?` here: every host already processed above has a real outcome sitting in
    // `outcomes` (upgraded, refreshed, or a per-host failure) that the caller still needs to
    // print and audit. Propagating a mesh-wide push failure at this point discarded that
    // already-completed, audit-worthy work along with it -- the caller's `?` on this whole
    // function would abort before it ever reached its own audit-recording loop, so a mid-run
    // failure here silently erased the audit trail for hosts that had already been upgraded.
    // Its own failure is instead returned as a "problem" the caller folds into whether the
    // overall command fails, matching `jiji server setup`'s `?`-propagated posture for the same
    // failure rather than only ever warning about it.
    let mut problems = Vec::new();
    match crate::commands::network::membership::push_membership_everywhere(
        config,
        ssh,
        &records,
        &target_names,
    )
    .await
    {
        Ok(push_outcome) => {
            for (name, error) in &push_outcome.unreachable {
                Ui::say(&format!("{name}: membership not yet current ({error})"), 1);
            }
        }
        Err(error) => {
            let message = format!("could not push membership to the rest of the mesh: {error}");
            Ui::warn(&format!(
                "{message}. Hosts upgraded above were still upgraded; run `jiji server upgrade` again once this is resolved."
            ));
            problems.push(message);
        }
    }

    Ok((outcomes, problems))
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
