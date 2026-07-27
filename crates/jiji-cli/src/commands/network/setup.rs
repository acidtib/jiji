use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::path::Path;

use anyhow::Context;
use jiji_config::{validate_config, Config, ContainerEngine, NamedServer};
use jiji_network::{
    ActiveSlotState, DnsRecord, Ipv4Cidr, NetworkPlan, NetworkPlanner, ServerPlan,
    ServiceNatArtifacts,
};
use jiji_ssh::{CommandResult, SshPool, SshSession};
use jiji_tui::Ui;
use sha2::{Digest, Sha256};

use super::bridge::{BridgeMigration, BridgeProvisioner};
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

pub(crate) fn service_nat_generations(slug: &str) -> String {
    format!("{}/service-nat-generations", network_dir(slug))
}

pub(crate) fn service_nat_current(slug: &str) -> String {
    format!("{}/service-nat-current", network_dir(slug))
}

fn dns_generations(slug: &str) -> String {
    format!("{}/dns-generations", network_dir(slug))
}

fn dns_current(slug: &str) -> String {
    format!("{}/dns-current", network_dir(slug))
}

// Bumped because this change makes every existing host's generation and artifact layout
// incompatible (project-scoped paths, per-project WireGuard/bridge names) -- an intentional
// breaking change, not a mistake; every already-provisioned host needs `jiji server teardown` +
// `jiji server setup` after upgrading.
const NETWORK_ARTIFACT_VERSION: u32 = 4;

struct ConnectedHost {
    name: String,
    session: SshSession,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct InstalledGeneration {
    network: Option<String>,
    dns: Option<String>,
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
    apply(&config, &plan, &target_names).await
}

pub(crate) async fn reconcile_for_deploy(
    config: &Config,
    plan: &NetworkPlan,
) -> anyhow::Result<()> {
    let target_names = plan.servers.keys().cloned().collect::<BTreeSet<_>>();
    let hosts = connect_all(config).await.context(
        "Could not reach every configured server for automatic network reconciliation. Restore SSH access and retry `jiji deploy`.",
    )?;
    let mut stale_hosts = Vec::new();
    for host in &hosts {
        match crate::network_guard::generation_is_current(&host.session, plan).await {
            Ok(true) => {}
            Ok(false) => stale_hosts.push(host.name.clone()),
            Err(error) => {
                close_all(&hosts).await;
                return Err(error).with_context(|| {
                    format!(
                        "Could not inspect the installed network generation on '{}'. Restore SSH access and retry `jiji deploy`.",
                        host.name
                    )
                });
            }
        }
    }

    if stale_hosts.is_empty() {
        close_all(&hosts).await;
        return Ok(());
    }

    Ui::section("Network Reconciliation:");
    Ui::say(
        &format!(
            "Network topology changed; applying generation {} to all configured servers.",
            &plan.generation[..12]
        ),
        1,
    );
    print_network_requirements(config, plan);
    let result = apply_connected(config, plan, &target_names, &hosts).await;
    close_all(&hosts).await;
    result.context(format!(
        "Automatic network reconciliation failed for stale host(s): {}. Fix the reported network error and retry `jiji deploy`.",
        stale_hosts.join(", ")
    ))
}

async fn apply(
    config: &Config,
    plan: &NetworkPlan,
    target_names: &BTreeSet<String>,
) -> anyhow::Result<()> {
    Ui::say(
        &format!(
            "Applying generation {} to: {}",
            &plan.generation[..12],
            target_names.iter().cloned().collect::<Vec<_>>().join(", ")
        ),
        1,
    );
    print_network_requirements(config, plan);
    let hosts = connect_all(config).await?;
    let result = apply_connected(config, plan, target_names, &hosts).await;
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
        Ui::warn(
            "network.container_cidr/management_cidr are unset, so this project uses jiji's shared default address ranges -- if another project also uses the defaults on the same host, their subnets could collide (rare, but not negligible past a handful of co-located projects). Consider setting distinct ranges in jiji.yml if you expect multiple projects on one server.",
        );
    }
}

async fn apply_connected(
    config: &Config,
    plan: &NetworkPlan,
    target_names: &BTreeSet<String>,
    hosts: &[ConnectedHost],
) -> anyhow::Result<()> {
    let slug = jiji_network::systemd_unit_slug(&plan.project);
    let mut migrations = BTreeMap::new();

    Ui::section("Preflight:");
    for host in hosts
        .iter()
        .filter(|host| target_names.contains(&host.name))
    {
        require_root(&host.session).await?;
        ensure_engine_available(&host.session, config.builder.engine).await?;
        if let Some(migration) = inspect_conflicts(
            &host.session,
            &host.name,
            &plan.servers[&host.name],
            plan,
            config.builder.engine,
        )
        .await?
        {
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
    let artifact_generation = artifact_generation(&plan.generation, &public_keys);

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
        )
        .await
        .with_context(|| {
            format!(
                "Could not stage generation {} on '{}'. No host generation was activated. Fix the reported error and rerun `jiji network setup`.",
                plan.generation, host.name
            )
        })?;
        Ui::say(&format!("{}: generation staged", host.name), 1);
    }

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
    let rollback_context = RollbackContext {
        previous: &previous,
        migrations: &migrations,
        config,
        plan,
        engine: config.builder.engine,
    };

    Ui::section("Activating Network:");
    let mut attempted = Vec::new();
    for host in &target_hosts {
        attempted.push(*host);
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
            config.builder.engine,
            &plan.servers[&host.name],
            &plan.generation,
            &artifact_generation,
            &slug,
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
                if let Err(error) = crate::proxy_ingress::ensure_ingress_rule(
                    &host.session,
                    plan.servers[&host.name].proxy_address,
                )
                .await
                {
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

    Ui::section("Verifying Network:");
    for host in &target_hosts {
        if let Err(error) = verify_host(&host.session, &plan.servers[&host.name], plan).await {
            return rollback_transaction(
                &attempted,
                &rollback_context,
                &host.name,
                "verification",
                error,
            )
            .await;
        }
        Ui::say(
            &format!("{}: WireGuard, bridge, routes, and DNS ready", host.name),
            1,
        );
    }

    Ui::section("Reconciling Service Mappings:");
    for host in &target_hosts {
        crate::service_network::reconcile_slots(&host.session, plan, &host.name)
            .await
            .with_context(|| {
                format!(
                    "Network generation {} is active on '{}', but stale service VIP mappings could not be reconciled. Fix the reported error and rerun `jiji network setup`.",
                    plan.generation, host.name
                )
            })?;
        Ui::say(
            &format!("{}: service mappings match configured topology", host.name),
            1,
        );
        if proxy_container_exists(&host.session, config.builder.engine).await? {
            reconcile_proxy_routes_after_migration(&host.session, config, plan, &host.name, None)
                .await?;
            Ui::say(
                &format!("{}: proxy routes match planned addresses", host.name),
                1,
            );
        }
    }

    Ui::success("Private network setup completed.");
    Ok(())
}

async fn proxy_container_exists(
    session: &SshSession,
    engine: ContainerEngine,
) -> anyhow::Result<bool> {
    let command = format!("{engine} container inspect kamal-proxy >/dev/null 2>&1");
    let result = session.execute(&command).await?;
    Ok(result.success)
}

async fn reconcile_proxy_routes_after_migration(
    session: &SshSession,
    config: &Config,
    plan: &NetworkPlan,
    server_name: &str,
    previous: Option<&BridgeMigration>,
) -> anyhow::Result<()> {
    let state = crate::service_network::load_active_slots(session, plan).await?;
    for endpoint in plan
        .endpoints
        .values()
        .filter(|endpoint| endpoint.server == server_name)
    {
        let Some(slot) = state.active_slot(&endpoint.identity) else {
            continue;
        };
        let service = &config.services[&endpoint.service];
        for mut target in crate::proxy_routes::targets_for_service(
            &plan.project,
            &endpoint.service,
            service.proxy.as_ref(),
            endpoint,
            slot,
        ) {
            if let Some(previous) = previous {
                let name = crate::container_runtime::container_name(
                    &plan.project,
                    &endpoint.service,
                    slot,
                );
                if let Some(address) = previous.previous_container_address(&name) {
                    target.address = address;
                }
            }
            crate::proxy_routes::deploy_route(session, config.builder.engine, &target).await?;
        }
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
) -> anyhow::Result<Option<BridgeMigration>> {
    let network_command = BridgeProvisioner::network_inspection_command(engine);
    let command = format!(
        "ip -o -4 route show table all; {network_command}; \
         wg show all listen-port 2>/dev/null | sed 's/^/PORT /' || true; \
         ip -o -4 address show | sed 's/^/ADDR /'"
    );
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)?;

    for line in result.stdout.lines() {
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
        "Server '{server_name}' already has a jiji-managed resource ('{other_interface_or_network}') using {what} '{value}', which this project also needs. This is a hash collision between two independent jiji projects sharing the same default network ranges on this host -- set a distinct `network.container_cidr`/`management_cidr` for one of them in `jiji.yml` and retry. This becomes more likely as more projects share a host with default ranges; consider setting distinct ranges proactively."
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
  DEBIAN_FRONTEND=noninteractive apt-get install -y -qq wireguard-tools dnsmasq-base dnsutils iptables nftables; \
elif command -v dnf >/dev/null 2>&1; then \
  dnf install -y wireguard-tools dnsmasq bind-utils iptables nftables; \
else \
  echo 'Unsupported package manager. Install wireguard-tools, dnsmasq, DNS query tools, iptables, and nftables manually.' >&2; exit 1; \
fi; \
install -d -m 0700 /etc/jiji/network; install -d -m 0700 {project_dir}; install -d -m 0700 /etc/wireguard"
    );
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)
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

async fn stage_host(
    session: &SshSession,
    engine: ContainerEngine,
    plan: &NetworkPlan,
    server: &ServerPlan,
    public_keys: &BTreeMap<String, String>,
    artifact_generation: &str,
    slug: &str,
) -> anyhow::Result<()> {
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
    stage_dns_generation(
        session,
        artifact_generation,
        &render_dns_config(server, &plan.dns_records),
        slug,
    )
    .await?;
    write_generation_file(
        session,
        &format!("{generation_dir}/generation"),
        "0644",
        &format!("{}\n", plan.generation),
    )
    .await?;
    let empty_nat = ServiceNatArtifacts::render(plan, &ActiveSlotState::default())?;
    initialize_service_nat(session, &empty_nat, slug).await?;
    write_empty_nat_if_inactive(session, &empty_nat.nftables, slug).await?;
    write_staged_file(
        session,
        &format!("{}/restore-service-nat.sh", network_dir(slug)),
        "0750",
        &render_service_nat_restore(plan),
    )
    .await?;
    write_staged_file(
        session,
        &format!("/etc/systemd/system/jiji-network-restore-{slug}.service"),
        "0644",
        &bridge.render_systemd_unit(),
    )
    .await?;
    if engine == ContainerEngine::Podman {
        // One drop-in file per project rather than one shared, edited-in-place file: turns a
        // stateful merge problem (tracking every project's own `After=`/`Requires=` entry inside
        // one file) into a simple idempotent add/remove-file problem, matching every other
        // per-project resource here. `podman-restart.service` itself is host-global, so it picks
        // up every project's drop-in automatically.
        write_staged_file(
            session,
            &format!("/etc/systemd/system/podman-restart.service.d/jiji-network-{slug}.conf"),
            "0644",
            &format!(
                "[Unit]\nAfter=jiji-network-restore-{slug}.service\nRequires=jiji-network-restore-{slug}.service\n\n[Service]\nExecStartPost=/usr/bin/podman start --all --filter restart-policy=unless-stopped\n"
            ),
        )
        .await?;
    }
    write_staged_file(
        session,
        &format!("/etc/systemd/system/jiji-service-nat-{slug}.service"),
        "0644",
        &render_service_nat_unit(slug),
    )
    .await?;
    write_staged_file(
        session,
        &format!("/etc/systemd/system/jiji-dns-{slug}.service"),
        "0644",
        &render_dns_unit(slug),
    )
    .await?;
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

async fn write_file_if_absent(
    session: &SshSession,
    path: &str,
    mode: &str,
    content: &str,
) -> anyhow::Result<()> {
    let command = format!(
        "set -eu; if test -e {path}; then cat >/dev/null; else install -D -m {mode} /dev/stdin {path}; fi"
    );
    let result = session
        .execute_with_input(&command, content.as_bytes())
        .await?;
    ensure_success(session, &command, &result)
}

async fn initialize_service_nat(
    session: &SshSession,
    artifacts: &ServiceNatArtifacts,
    slug: &str,
) -> anyhow::Result<()> {
    let initial = format!("{}/initial", service_nat_generations(slug));
    let current = service_nat_current(slug);
    let command = format!("install -d -m 0750 {initial}");
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)?;
    write_file_if_absent(
        session,
        &format!("{initial}/active-slots"),
        "0644",
        &artifacts.state,
    )
    .await?;
    write_file_if_absent(
        session,
        &format!("{initial}/service-nat.nft"),
        "0644",
        &artifacts.nftables,
    )
    .await?;
    let command = format!(
        "set -eu; if ! test -L {current}; then ln -s {initial} {current}.new; mv -T {current}.new {current}; fi"
    );
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)
}

async fn write_empty_nat_if_inactive(
    session: &SshSession,
    nftables: &str,
    slug: &str,
) -> anyhow::Result<()> {
    let current = service_nat_current(slug);
    let command = format!(
        "set -eu; if test -s {current}/active-slots; then cat >/dev/null; else install -m 0644 /dev/stdin {current}/service-nat.nft; fi"
    );
    let result = session
        .execute_with_input(&command, nftables.as_bytes())
        .await?;
    ensure_success(session, &command, &result)
}

async fn stage_dns_generation(
    session: &SshSession,
    generation: &str,
    config: &str,
    slug: &str,
) -> anyhow::Result<()> {
    let directory = format!("{}/{generation}", dns_generations(slug));
    let command = format!("install -d -m 0755 {directory}");
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)?;
    let path = format!("{directory}/dns.conf");
    write_generation_file(session, &path, "0644", config).await?;
    let command = format!("dnsmasq --test --conf-file={path}");
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)
}

async fn capture_installed_generation(
    session: &SshSession,
    slug: &str,
) -> anyhow::Result<InstalledGeneration> {
    let current = network_current(slug);
    let dns_current = dns_current(slug);
    let command = format!(
        "set -eu; \
         if test -L {current}; then \
           readlink -f {current}; \
         else \
           printf '%s\\n' -; \
         fi; \
         if test -L {dns_current}; then readlink -f {dns_current}; else printf '%s\\n' -; fi"
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
    if lines.len() != 2 {
        anyhow::bail!("expected two generation paths, received {}", lines.len());
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
        dns: parse(lines[1], &format!("{}/", dns_generations(slug)))?,
    })
}

async fn activate_host(
    session: &SshSession,
    engine: ContainerEngine,
    server: &ServerPlan,
    generation: &str,
    artifact_generation: &str,
    slug: &str,
) -> anyhow::Result<()> {
    let iface = &server.wireguard_interface;
    let expected_routes = server
        .peers
        .iter()
        .flat_map(|peer| peer.allowed_ips.iter())
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let route_sync = if expected_routes.is_empty() {
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
    };
    let network_generation = format!("{}/{artifact_generation}", network_generations(slug));
    let dns_generation = format!("{}/{artifact_generation}", dns_generations(slug));
    let network_current = network_current(slug);
    let dns_current = dns_current(slug);
    let network_dir = network_dir(slug);
    let wireguard_config_path = wireguard_config_path(iface);
    let units = format!(
        "wg-quick@{iface}.service jiji-network-restore-{slug}.service jiji-service-nat-{slug}.service jiji-dns-{slug}.service"
    );
    let enable_engine_restart = match engine {
        ContainerEngine::Docker => "",
        ContainerEngine::Podman => "systemctl enable podman-restart.service >/dev/null; ",
    };
    let restart_engine_containers = match engine {
        ContainerEngine::Docker => "",
        ContainerEngine::Podman => "systemctl restart podman-restart.service; ",
    };
    let command = format!(
        "set -eu; \
         sysctl --system >/dev/null; \
         systemctl daemon-reload; \
         systemctl enable {units} >/dev/null; \
         {enable_engine_restart}\
         test -s {network_generation}/wireguard.conf; \
         test -x {network_generation}/restore.sh; \
         test \"$(cat {network_generation}/generation)\" = '{generation}'; \
         test -s {dns_generation}/dns.conf; \
         ln -sfn {network_generation} {network_current}.new; \
         mv -Tf {network_current}.new {network_current}; \
         ln -sfn {dns_generation} {dns_current}.new; \
         mv -Tf {dns_current}.new {dns_current}; \
         ln -sfn {network_current}/wireguard.conf {wireguard_config_path}.new; \
         mv -Tf {wireguard_config_path}.new {wireguard_config_path}; \
         ln -sfn {network_current}/restore.sh {network_dir}/restore.sh.new; \
         mv -Tf {network_dir}/restore.sh.new {network_dir}/restore.sh; \
         ln -sfn {network_current}/generation {network_dir}/generation.new; \
         mv -Tf {network_dir}/generation.new {network_dir}/generation; \
         if systemctl is-active --quiet wg-quick@{iface}.service && ip -o -4 address show dev {iface} | grep -F ' {management}/32 ' >/dev/null; then \
           bash -c 'wg syncconf {iface} <(wg-quick strip {wireguard_config_path})'; \
           {route_sync}; \
         else \
           systemctl restart wg-quick@{iface}.service; \
         fi; \
         systemctl restart jiji-network-restore-{slug}.service; \
         {restart_engine_containers}\
         systemctl restart jiji-service-nat-{slug}.service; \
         systemctl restart jiji-dns-{slug}.service",
        management = server.management_address,
    );
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)
}

struct RollbackContext<'a> {
    previous: &'a BTreeMap<String, InstalledGeneration>,
    migrations: &'a BTreeMap<String, BridgeMigration>,
    config: &'a Config,
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
    let wireguard_interface = jiji_network::wireguard_interface_name(&context.plan.project);
    let mut rollback_failures = Vec::new();
    for host in attempted.iter().rev() {
        let state = &context.previous[&host.name];
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
                    if let Err(error) =
                        crate::proxy_ingress::ensure_ingress_rule(&host.session, address).await
                    {
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
            &wireguard_interface,
            context.engine,
            &context.plan.project,
        )
        .await;
        if let Err(error) = rollback {
            rollback_failures.push(format!("{}: {error}", host.name));
            continue;
        }
        if let Some(migration) = context
            .migrations
            .get(&host.name)
            .filter(|migration| migration.includes_proxy())
        {
            if let Err(error) = reconcile_proxy_routes_after_migration(
                &host.session,
                context.config,
                context.plan,
                &host.name,
                Some(migration),
            )
            .await
            {
                rollback_failures.push(format!(
                    "{}: previous network was restored but proxy routes were not: {error}",
                    host.name
                ));
                continue;
            }
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
    wireguard_interface: &str,
    engine: ContainerEngine,
    project: &str,
) -> anyhow::Result<()> {
    let command = render_rollback_command(state, slug, wireguard_interface);
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)?;

    // Confirmed live: a first-install activation can fail *inside* `restore.sh` (run via
    // `systemctl restart jiji-network-restore-{slug}.service`) after it has already created
    // engine-level resources, including the bridge network.
    // `render_rollback_command` only reverts jiji's own compiled-state symlinks/units; it has no
    // way to know what a partially-run *external* script did. On a first install (no previous
    // generation to fall back to -- if there were, restarting that generation's own idempotent
    // `restore.sh` already reconciles engine state correctly), also remove whatever engine
    // resources this attempt may have created, using the exact same primitives `jiji server
    // teardown` uses, so a failed `jiji network setup` never leaves orphaned infrastructure behind.
    if state.network.is_none() {
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
    wireguard_interface: &str,
) -> String {
    let network_current = network_current(slug);
    let network_dir = network_dir(slug);
    let wireguard_config_path = wireguard_config_path(wireguard_interface);
    let network = match &state.network {
        Some(path) => format!(
            "ln -sfn {path} {network_current}.new; \
             mv -Tf {network_current}.new {network_current}; \
             ln -sfn {network_current}/wireguard.conf {wireguard_config_path}.new; \
             mv -Tf {wireguard_config_path}.new {wireguard_config_path}; \
             ln -sfn {network_current}/restore.sh {network_dir}/restore.sh.new; \
             mv -Tf {network_dir}/restore.sh.new {network_dir}/restore.sh; \
             ln -sfn {network_current}/generation {network_dir}/generation.new; \
             mv -Tf {network_dir}/generation.new {network_dir}/generation"
        ),
        None => format!(
            "systemctl stop jiji-dns-{slug}.service jiji-service-nat-{slug}.service jiji-network-restore-{slug}.service wg-quick@{wireguard_interface}.service 2>/dev/null || true; \
             rm -f {network_current} {wireguard_config_path} {network_dir}/restore.sh {network_dir}/generation"
        ),
    };
    let dns_current = dns_current(slug);
    let dns = match &state.dns {
        Some(path) => {
            format!("ln -sfn {path} {dns_current}.new; mv -Tf {dns_current}.new {dns_current}")
        }
        None => format!("rm -f {dns_current}"),
    };
    let restart = if state.network.is_some() {
        format!(
            "systemctl daemon-reload; \
             systemctl restart wg-quick@{wireguard_interface}.service; \
             systemctl restart jiji-network-restore-{slug}.service; \
             systemctl restart jiji-service-nat-{slug}.service; \
             systemctl restart jiji-dns-{slug}.service"
        )
    } else {
        "true".to_string()
    };
    format!("set -eu; {network}; {dns}; {restart}")
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

async fn verify_host(
    session: &SshSession,
    server: &ServerPlan,
    plan: &NetworkPlan,
) -> anyhow::Result<()> {
    let dns_check = plan
        .dns_records
        .values()
        .next()
        .map(|record| {
            format!(
                "test -n \"$(dig +time=2 +tries=1 +short @{} {})\"",
                server.dns_address,
                record.name.trim_end_matches('.')
            )
        })
        .unwrap_or_else(|| {
            format!(
                "dig +time=2 +tries=1 @{} . SOA >/dev/null",
                server.dns_address
            )
        });
    let peer_checks = server
        .peers
        .iter()
        .map(|peer| format!("ping -c 1 -W 3 {} >/dev/null", peer.management_address))
        .collect::<Vec<_>>()
        .join("; ");
    let slug = jiji_network::systemd_unit_slug(&plan.project);
    let network_dir = network_dir(&slug);
    let command = format!(
        "set -eu; \
         wg show {wireguard_interface} >/dev/null; \
         ip link show {bridge_interface} >/dev/null; \
         ip -4 address show dev {bridge_interface} | grep -F '{bridge_gateway}' >/dev/null; \
         test \"$(cat {network_dir}/generation)\" = '{generation}'; \
         systemctl is-active --quiet jiji-network-restore-{slug}.service; \
         systemctl is-active --quiet jiji-service-nat-{slug}.service; \
         systemctl is-active --quiet jiji-dns-{slug}.service; \
         {dns_check}; \
         {peer_checks}",
        wireguard_interface = server.wireguard_interface,
        bridge_interface = server.bridge_interface,
        bridge_gateway = server.bridge_gateway,
        generation = plan.generation,
    );
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
        let public_key = public_keys.get(&peer.server).ok_or_else(|| {
            anyhow::anyhow!(
                "No WireGuard public key was collected for server '{}'",
                peer.server
            )
        })?;
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

fn render_dns_config(server: &ServerPlan, records: &BTreeMap<String, DnsRecord>) -> String {
    let mut output = format!(
        "port=53\nno-hosts\nlocal=/jiji/\nbind-dynamic\nlisten-address={}\n",
        server.dns_address
    );
    for record in records.values() {
        for address in &record.addresses {
            output.push_str(&format!(
                "host-record={},{}\n",
                record.name.trim_end_matches('.'),
                address
            ));
        }
    }
    output
}

fn render_dns_unit(slug: &str) -> String {
    let current = dns_current(slug);
    format!(
        "[Unit]\nDescription=jiji static service DNS\nAfter=jiji-network-restore-{slug}.service jiji-service-nat-{slug}.service\nRequires=jiji-network-restore-{slug}.service jiji-service-nat-{slug}.service\nConditionPathExists={current}/dns.conf\n\n[Service]\nType=simple\nExecStart=/usr/sbin/dnsmasq --keep-in-foreground --conf-file={current}/dns.conf\nRestart=on-failure\nRestartSec=2\n\n[Install]\nWantedBy=multi-user.target\n"
    )
}

fn render_service_nat_restore(plan: &NetworkPlan) -> String {
    let slug = jiji_network::systemd_unit_slug(&plan.project);
    let current = service_nat_current(&slug);
    let table = jiji_network::service_nat_table_name(&plan.project);
    format!(
        "#!/bin/sh\nset -eu\nnft add table ip {table} 2>/dev/null || true\nnft --check --file {current}/service-nat.nft\nnft --file {current}/service-nat.nft\n"
    )
}

fn render_service_nat_unit(slug: &str) -> String {
    format!(
        "[Unit]\nDescription=Restore jiji service VIP mappings\nAfter=jiji-network-restore-{slug}.service\nRequires=jiji-network-restore-{slug}.service\nBefore=jiji-dns-{slug}.service\n\n[Service]\nType=oneshot\nExecStart={}/restore-service-nat.sh\nRemainAfterExit=yes\n\n[Install]\nWantedBy=multi-user.target\n",
        network_dir(slug)
    )
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
    fn dns_rendering_contains_all_replica_addresses() {
        let plan = plan();
        let rendered = render_dns_config(&plan.servers["app"], &plan.dns_records);
        assert!(rendered.contains("local=/jiji/"));
        assert!(rendered.contains("bind-dynamic"));
        assert!(!rendered.contains("no-resolv"));
        let web = &plan.dns_records["demo-web.jiji."];
        for address in &web.addresses {
            assert!(rendered.contains(&format!("host-record=demo-web.jiji,{address}")));
        }
    }

    #[test]
    fn dns_unit_reads_the_atomically_selected_generation() {
        let slug = jiji_network::systemd_unit_slug("demo");
        let rendered = render_dns_unit(&slug);
        let expected_current = dns_current(&slug);
        assert!(rendered.contains(&format!("ConditionPathExists={expected_current}/dns.conf")));
        assert!(rendered.contains(&format!("--conf-file={expected_current}/dns.conf")));
    }

    #[test]
    fn installed_generation_parser_rejects_paths_outside_managed_roots() {
        let slug = jiji_network::systemd_unit_slug("demo");
        let generations = network_generations(&slug);
        let dns_generations = dns_generations(&slug);
        let state = parse_installed_generation(
            &format!("{generations}/abc\n{dns_generations}/abc\n"),
            &slug,
        )
        .unwrap();
        assert_eq!(state.network_name(), "abc");
        assert!(
            parse_installed_generation(&format!("/tmp/abc\n{dns_generations}/abc\n"), &slug)
                .is_err()
        );
        assert!(parse_installed_generation(
            &format!("{generations}/abc extra\n{dns_generations}/abc\n"),
            &slug
        )
        .is_err());
    }

    #[test]
    fn rollback_selects_both_previous_generations_before_restarting_services() {
        let slug = jiji_network::systemd_unit_slug("demo");
        let wireguard_interface = jiji_network::wireguard_interface_name("demo");
        let state = InstalledGeneration {
            network: Some(format!("{}/old", network_generations(&slug))),
            dns: Some(format!("{}/old", dns_generations(&slug))),
        };
        let command = render_rollback_command(&state, &slug, &wireguard_interface);
        let network_switch = command
            .find(&format!("ln -sfn {}/old", network_generations(&slug)))
            .unwrap();
        let dns_switch = command
            .find(&format!("ln -sfn {}/old", dns_generations(&slug)))
            .unwrap();
        let restart = command.find("systemctl restart wg-quick").unwrap();
        assert!(network_switch < restart);
        assert!(dns_switch < restart);
        assert!(command.contains(&format!(
            "ln -sfn {}/wireguard.conf {}.new",
            network_current(&slug),
            wireguard_config_path(&wireguard_interface)
        )));
    }

    #[test]
    fn first_install_rollback_removes_only_jiji_managed_live_paths() {
        let slug = jiji_network::systemd_unit_slug("demo");
        let wireguard_interface = jiji_network::wireguard_interface_name("demo");
        let command = render_rollback_command(
            &InstalledGeneration {
                network: None,
                dns: None,
            },
            &slug,
            &wireguard_interface,
        );
        assert!(command.contains(&format!("systemctl stop jiji-dns-{slug}.service")));
        assert!(command.contains(&format!(
            "rm -f {} {} {}/restore.sh {}/generation",
            network_current(&slug),
            wireguard_config_path(&wireguard_interface),
            network_dir(&slug),
            network_dir(&slug),
        )));
        assert!(command.contains(&format!("rm -f {}", dns_current(&slug))));
        assert!(!command.contains("systemctl restart"));
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
}
