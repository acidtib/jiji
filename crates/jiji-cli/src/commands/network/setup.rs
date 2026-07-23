use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::path::Path;

use anyhow::Context;
use jiji_config::{load_config, validate_config, Config, ContainerEngine, NamedServer};
use jiji_network::{
    ActiveSlotState, DnsRecord, Ipv4Cidr, NetworkPlan, NetworkPlanner, ServerPlan,
    ServiceNatArtifacts,
};
use jiji_ssh::{CommandResult, SshPool, SshSession};
use jiji_tui::Ui;
use sha2::{Digest, Sha256};

use super::bridge::BridgeProvisioner;
use crate::ssh_adapter;

// `pub(crate)`: reused as-is by `crate::network_teardown`, the inverse of this module.
pub(crate) const PRIVATE_KEY_PATH: &str = "/etc/jiji/network/private.key";
pub(crate) const PUBLIC_KEY_PATH: &str = "/etc/jiji/network/public.key";
pub(crate) const WIREGUARD_CONFIG_PATH: &str = "/etc/wireguard/jiji0.conf";
pub(crate) const NETWORK_DIR: &str = "/etc/jiji/network";
const NETWORK_GENERATIONS: &str = "/etc/jiji/network/generations";
const NETWORK_CURRENT: &str = "/etc/jiji/network/current";
const SERVICE_NAT_GENERATIONS: &str = "/etc/jiji/network/service-nat-generations";
const SERVICE_NAT_CURRENT: &str = "/etc/jiji/network/service-nat-current";
const DNS_GENERATIONS: &str = "/etc/jiji/network/dns-generations";
const DNS_CURRENT: &str = "/etc/jiji/network/dns-current";
const NETWORK_ARTIFACT_VERSION: u32 = 3;

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
    let (config, path) = load_config(environment, config_path, &start)?;
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

async fn apply(
    config: &Config,
    plan: &NetworkPlan,
    target_names: &BTreeSet<String>,
) -> anyhow::Result<()> {
    if config.servers.is_empty() {
        anyhow::bail!("No servers are configured. Add at least one server and retry.");
    }
    let ssh = config.ssh.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "No `ssh:` section is configured. Add at least `ssh.user:` before running network setup."
        )
    })?;

    Ui::say(
        &format!(
            "Applying generation {} to: {}",
            &plan.generation[..12],
            target_names.iter().cloned().collect::<Vec<_>>().join(", ")
        ),
        1,
    );
    Ui::say(
        "All configured servers must be reachable during initial WireGuard key enrollment.",
        1,
    );
    Ui::say(
        "WireGuard UDP port 51820 must be allowed between the configured server public IPs.",
        1,
    );

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

    let result = apply_connected(config, plan, target_names, &hosts).await;
    close_all(&hosts).await;
    result
}

async fn apply_connected(
    config: &Config,
    plan: &NetworkPlan,
    target_names: &BTreeSet<String>,
    hosts: &[ConnectedHost],
) -> anyhow::Result<()> {
    Ui::section("Preflight:");
    for host in hosts
        .iter()
        .filter(|host| target_names.contains(&host.name))
    {
        require_root(&host.session).await?;
        ensure_engine_available(&host.session, config.builder.engine).await?;
        inspect_conflicts(
            &host.session,
            &host.name,
            &plan.servers[&host.name],
            plan,
            config.builder.engine,
        )
        .await?;
        Ui::say(&format!("{}: address ranges are available", host.name), 1);
    }

    Ui::section("Preparing Hosts:");
    for host in hosts
        .iter()
        .filter(|host| target_names.contains(&host.name))
    {
        install_prerequisites(&host.session).await?;
        ensure_keypair(&host.session).await?;
        Ui::say(
            &format!("{}: prerequisites and host key ready", host.name),
            1,
        );
    }

    let mut public_keys = BTreeMap::new();
    for host in hosts {
        let result = host
            .session
            .execute(&format!(
                "test -s {PUBLIC_KEY_PATH} && cat {PUBLIC_KEY_PATH}"
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
            capture_installed_generation(&host.session).await?,
        );
    }

    Ui::section("Activating Network:");
    let mut attempted = Vec::new();
    for host in &target_hosts {
        attempted.push(*host);
        if let Err(error) = activate_host(
            &host.session,
            config.builder.engine,
            &plan.servers[&host.name],
            &plan.generation,
            &artifact_generation,
        )
        .await
        {
            return rollback_transaction(&attempted, &previous, &host.name, "activation", error)
                .await;
        }
        Ui::say(&format!("{}: generation activated", host.name), 1);
    }

    Ui::section("Verifying Network:");
    for host in &target_hosts {
        if let Err(error) = verify_host(&host.session, &plan.servers[&host.name], plan).await {
            return rollback_transaction(&attempted, &previous, &host.name, "verification", error)
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
    }

    Ui::success("Private network setup completed.");
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

async fn inspect_conflicts(
    session: &SshSession,
    server_name: &str,
    server_plan: &ServerPlan,
    plan: &NetworkPlan,
    engine: ContainerEngine,
) -> anyhow::Result<()> {
    let network_command = BridgeProvisioner::network_inspection_command(engine);
    let command = format!("ip -o -4 route show table all; {network_command}");
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
                if name == "jiji" && cidr == server_plan.container_subnet {
                    continue;
                }
                reject_overlap(server_name, "container network", cidr, plan)?;
            }
            continue;
        }

        if line.contains(" dev jiji0 ") || line.contains(" dev jiji ") {
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

    let bridge_validation =
        BridgeProvisioner::new(engine, plan, server_plan).render_existing_validation_command();
    let result = session.execute(&bridge_validation).await?;
    ensure_success(session, &bridge_validation, &result)?;
    Ok(())
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

async fn install_prerequisites(session: &SshSession) -> anyhow::Result<()> {
    const COMMAND: &str = "set -eu; \
if command -v apt-get >/dev/null 2>&1; then \
  apt-get update -qq; \
  DEBIAN_FRONTEND=noninteractive apt-get install -y -qq wireguard-tools dnsmasq-base dnsutils iptables nftables busybox-static; \
elif command -v dnf >/dev/null 2>&1; then \
  dnf install -y wireguard-tools dnsmasq bind-utils iptables nftables busybox; \
else \
  echo 'Unsupported package manager. Install wireguard-tools, dnsmasq, DNS query tools, iptables, nftables, and static BusyBox manually.' >&2; exit 1; \
fi; \
install -d -m 0700 /etc/jiji/network; install -d -m 0700 /etc/wireguard";
    let result = session.execute(COMMAND).await?;
    ensure_success(session, COMMAND, &result)
}

async fn ensure_keypair(session: &SshSession) -> anyhow::Result<()> {
    let command = format!(
        "set -eu; umask 077; \
         test -s {PRIVATE_KEY_PATH} || wg genkey > {PRIVATE_KEY_PATH}; \
         wg pubkey < {PRIVATE_KEY_PATH} > {PUBLIC_KEY_PATH}; \
         chmod 0600 {PRIVATE_KEY_PATH}; chmod 0644 {PUBLIC_KEY_PATH}"
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
) -> anyhow::Result<()> {
    let bridge = BridgeProvisioner::new(engine, plan, server);
    let generation_dir = format!("{NETWORK_GENERATIONS}/{artifact_generation}");
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
    let finalize_wireguard = format!(
        "set -eu; private_key=$(cat {PRIVATE_KEY_PATH}); \
         sed \"s|__JIJI_PRIVATE_KEY__|$private_key|\" {generation_dir}/wireguard.conf.input > {generation_dir}/wireguard.conf.new; \
         chmod 0600 {generation_dir}/wireguard.conf.new; \
         cp {generation_dir}/wireguard.conf.new /etc/wireguard/jiji0-staged.conf; \
         wg-quick strip /etc/wireguard/jiji0-staged.conf >/dev/null; \
         if test -e {generation_dir}/wireguard.conf; then \
           cmp -s {generation_dir}/wireguard.conf.new {generation_dir}/wireguard.conf || {{ echo 'Staged WireGuard content differs inside an existing immutable generation. Upgrade jiji or report an artifact-version bug.' >&2; exit 1; }}; \
           rm -f {generation_dir}/wireguard.conf.new; \
         else \
           mv {generation_dir}/wireguard.conf.new {generation_dir}/wireguard.conf; \
         fi; \
         rm -f {generation_dir}/wireguard.conf.input /etc/wireguard/jiji0-staged.conf"
    );
    let result = session.execute(&finalize_wireguard).await?;
    ensure_success(session, &finalize_wireguard, &result)?;

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
    initialize_service_nat(session, &empty_nat).await?;
    write_empty_nat_if_inactive(session, &empty_nat.nftables).await?;
    write_staged_file(
        session,
        &format!("{NETWORK_DIR}/restore-service-nat.sh"),
        "0750",
        &render_service_nat_restore(),
    )
    .await?;
    write_staged_file(
        session,
        "/etc/systemd/system/jiji-network-restore.service",
        "0644",
        &bridge.render_systemd_unit(),
    )
    .await?;
    if engine == ContainerEngine::Podman {
        write_staged_file(
            session,
            "/etc/systemd/system/podman-restart.service.d/jiji-network.conf",
            "0644",
            "[Unit]\nAfter=jiji-network-restore.service\nRequires=jiji-network-restore.service\n",
        )
        .await?;
    }
    write_staged_file(
        session,
        "/etc/systemd/system/jiji-service-nat.service",
        "0644",
        &render_service_nat_unit(),
    )
    .await?;
    write_staged_file(
        session,
        "/etc/systemd/system/jiji-dns.service",
        "0644",
        &render_dns_unit(),
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
) -> anyhow::Result<()> {
    let initial = format!("{SERVICE_NAT_GENERATIONS}/initial");
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
        "set -eu; if ! test -L {SERVICE_NAT_CURRENT}; then ln -s {initial} {SERVICE_NAT_CURRENT}.new; mv -T {SERVICE_NAT_CURRENT}.new {SERVICE_NAT_CURRENT}; fi"
    );
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)
}

async fn write_empty_nat_if_inactive(session: &SshSession, nftables: &str) -> anyhow::Result<()> {
    let command = format!(
        "set -eu; if test -s {SERVICE_NAT_CURRENT}/active-slots; then cat >/dev/null; else install -m 0644 /dev/stdin {SERVICE_NAT_CURRENT}/service-nat.nft; fi"
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
) -> anyhow::Result<()> {
    let directory = format!("{DNS_GENERATIONS}/{generation}");
    let command = format!("install -d -m 0755 {directory}");
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)?;
    let path = format!("{directory}/dns.conf");
    write_generation_file(session, &path, "0644", config).await?;
    let command = format!("dnsmasq --test --conf-file={path}");
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)
}

async fn capture_installed_generation(session: &SshSession) -> anyhow::Result<InstalledGeneration> {
    let command = format!(
        "set -eu; \
         if test -L {NETWORK_CURRENT}; then \
           readlink -f {NETWORK_CURRENT}; \
         else \
           printf '%s\\n' -; \
         fi; \
         if test -L {DNS_CURRENT}; then readlink -f {DNS_CURRENT}; else printf '%s\\n' -; fi"
    );
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)?;
    parse_installed_generation(&result.stdout).with_context(|| {
        format!(
            "Host {} returned invalid installed network generation state",
            session.host()
        )
    })
}

fn parse_installed_generation(value: &str) -> anyhow::Result<InstalledGeneration> {
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
        network: parse(lines[0], &format!("{NETWORK_GENERATIONS}/"))?,
        dns: parse(lines[1], &format!("{DNS_GENERATIONS}/"))?,
    })
}

async fn activate_host(
    session: &SshSession,
    engine: ContainerEngine,
    server: &ServerPlan,
    generation: &str,
    artifact_generation: &str,
) -> anyhow::Result<()> {
    let expected_routes = server
        .peers
        .iter()
        .flat_map(|peer| peer.allowed_ips.iter())
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let route_sync = if expected_routes.is_empty() {
        "for route in $(ip -4 route show dev jiji0 | awk '{print $1}'); do ip route del \"$route\" dev jiji0; done".to_string()
    } else {
        let replacements = expected_routes
            .iter()
            .map(|route| format!("ip route replace {route} dev jiji0"))
            .collect::<Vec<_>>()
            .join("; ");
        format!(
            "for route in $(ip -4 route show dev jiji0 | awk '{{print $1}}'); do case \"$route\" in {}) ;; *) ip route del \"$route\" dev jiji0 ;; esac; done; {replacements}",
            expected_routes.join("|")
        )
    };
    let network_generation = format!("{NETWORK_GENERATIONS}/{artifact_generation}");
    let dns_generation = format!("{DNS_GENERATIONS}/{artifact_generation}");
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
         systemctl enable wg-quick@jiji0.service jiji-network-restore.service jiji-service-nat.service jiji-dns.service >/dev/null; \
         {enable_engine_restart}\
         test -s {network_generation}/wireguard.conf; \
         test -x {network_generation}/restore.sh; \
         test \"$(cat {network_generation}/generation)\" = '{generation}'; \
         test -s {dns_generation}/dns.conf; \
         ln -sfn {network_generation} {NETWORK_CURRENT}.new; \
         mv -Tf {NETWORK_CURRENT}.new {NETWORK_CURRENT}; \
         ln -sfn {dns_generation} {DNS_CURRENT}.new; \
         mv -Tf {DNS_CURRENT}.new {DNS_CURRENT}; \
         ln -sfn {NETWORK_CURRENT}/wireguard.conf {WIREGUARD_CONFIG_PATH}.new; \
         mv -Tf {WIREGUARD_CONFIG_PATH}.new {WIREGUARD_CONFIG_PATH}; \
         ln -sfn {NETWORK_CURRENT}/restore.sh {NETWORK_DIR}/restore.sh.new; \
         mv -Tf {NETWORK_DIR}/restore.sh.new {NETWORK_DIR}/restore.sh; \
         ln -sfn {NETWORK_CURRENT}/generation {NETWORK_DIR}/generation.new; \
         mv -Tf {NETWORK_DIR}/generation.new {NETWORK_DIR}/generation; \
         if systemctl is-active --quiet wg-quick@jiji0.service && ip -o -4 address show dev jiji0 | grep -F ' {management}/32 ' >/dev/null; then \
           bash -c 'wg syncconf jiji0 <(wg-quick strip {WIREGUARD_CONFIG_PATH})'; \
           {route_sync}; \
         else \
           systemctl restart wg-quick@jiji0.service; \
         fi; \
         systemctl restart jiji-network-restore.service; \
         {restart_engine_containers}\
         systemctl restart jiji-service-nat.service; \
         systemctl restart jiji-dns.service",
        management = server.management_address,
    );
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)
}

async fn rollback_transaction(
    attempted: &[&ConnectedHost],
    previous: &BTreeMap<String, InstalledGeneration>,
    failed_host: &str,
    phase: &str,
    cause: anyhow::Error,
) -> anyhow::Result<()> {
    Ui::error(&format!(
        "Network {phase} failed on {failed_host}: {cause}. Rolling back attempted hosts."
    ));
    let mut rollback_failures = Vec::new();
    for host in attempted.iter().rev() {
        let state = &previous[&host.name];
        match rollback_host(&host.session, state).await {
            Ok(()) => Ui::say(
                &format!(
                    "{}: restored generation {}",
                    host.name,
                    state.network_name()
                ),
                1,
            ),
            Err(error) => rollback_failures.push(format!("{}: {error}", host.name)),
        }
    }
    if rollback_failures.is_empty() {
        anyhow::bail!(
            "Network {phase} failed on '{failed_host}' and all attempted hosts were rolled back. Fix the reported error and rerun `jiji network setup`."
        );
    }
    anyhow::bail!(
        "Network {phase} failed on '{failed_host}'. Rollback also failed on: {}. Run `jiji network setup` after restoring SSH access, and inspect `/etc/jiji/network/current` on those hosts.",
        rollback_failures.join("; ")
    )
}

async fn rollback_host(session: &SshSession, state: &InstalledGeneration) -> anyhow::Result<()> {
    let command = render_rollback_command(state);
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)
}

fn render_rollback_command(state: &InstalledGeneration) -> String {
    let network = match &state.network {
        Some(path) => format!(
            "ln -sfn {path} {NETWORK_CURRENT}.new; \
             mv -Tf {NETWORK_CURRENT}.new {NETWORK_CURRENT}; \
             ln -sfn {NETWORK_CURRENT}/wireguard.conf {WIREGUARD_CONFIG_PATH}.new; \
             mv -Tf {WIREGUARD_CONFIG_PATH}.new {WIREGUARD_CONFIG_PATH}; \
             ln -sfn {NETWORK_CURRENT}/restore.sh {NETWORK_DIR}/restore.sh.new; \
             mv -Tf {NETWORK_DIR}/restore.sh.new {NETWORK_DIR}/restore.sh; \
             ln -sfn {NETWORK_CURRENT}/generation {NETWORK_DIR}/generation.new; \
             mv -Tf {NETWORK_DIR}/generation.new {NETWORK_DIR}/generation"
        ),
        None => format!(
            "systemctl stop jiji-dns.service jiji-service-nat.service jiji-network-restore.service wg-quick@jiji0.service 2>/dev/null || true; \
             rm -f {NETWORK_CURRENT} {WIREGUARD_CONFIG_PATH} {NETWORK_DIR}/restore.sh {NETWORK_DIR}/generation"
        ),
    };
    let dns = match &state.dns {
        Some(path) => {
            format!("ln -sfn {path} {DNS_CURRENT}.new; mv -Tf {DNS_CURRENT}.new {DNS_CURRENT}")
        }
        None => format!("rm -f {DNS_CURRENT}"),
    };
    let restart = if state.network.is_some() {
        "systemctl daemon-reload; \
         systemctl restart wg-quick@jiji0.service; \
         systemctl restart jiji-network-restore.service; \
         systemctl restart jiji-service-nat.service; \
         systemctl restart jiji-dns.service"
    } else {
        "true"
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
    let command = format!(
        "set -eu; \
         wg show jiji0 >/dev/null; \
         ip link show jiji >/dev/null; \
         ip -4 address show dev jiji | grep -F '{}' >/dev/null; \
         test \"$(cat {NETWORK_DIR}/generation)\" = '{}'; \
         systemctl is-active --quiet jiji-network-restore.service; \
         systemctl is-active --quiet jiji-service-nat.service; \
         systemctl is-active --quiet jiji-dns.service; \
         {dns_check}; \
         {peer_checks}",
        server.bridge_gateway, plan.generation
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

fn render_dns_unit() -> String {
    "[Unit]\nDescription=jiji static service DNS\nAfter=jiji-network-restore.service jiji-service-nat.service\nRequires=jiji-network-restore.service jiji-service-nat.service\nConditionPathExists=/etc/jiji/network/dns-current/dns.conf\n\n[Service]\nType=simple\nExecStart=/usr/sbin/dnsmasq --keep-in-foreground --conf-file=/etc/jiji/network/dns-current/dns.conf\nRestart=on-failure\nRestartSec=2\n\n[Install]\nWantedBy=multi-user.target\n".to_string()
}

fn render_service_nat_restore() -> String {
    "#!/bin/sh\nset -eu\nnft add table ip jiji_service_nat 2>/dev/null || true\nnft --check --file /etc/jiji/network/service-nat-current/service-nat.nft\nnft --file /etc/jiji/network/service-nat-current/service-nat.nft\n".to_string()
}

fn render_service_nat_unit() -> String {
    "[Unit]\nDescription=Restore jiji service VIP mappings\nAfter=jiji-network-restore.service\nRequires=jiji-network-restore.service\nBefore=jiji-dns.service\n\n[Service]\nType=oneshot\nExecStart=/etc/jiji/network/restore-service-nat.sh\nRemainAfterExit=yes\n\n[Install]\nWantedBy=multi-user.target\n".to_string()
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
  web: { hosts: [app, data] }
  redis: { hosts: [data] }
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
        let rendered = render_dns_unit();
        assert!(rendered.contains("ConditionPathExists=/etc/jiji/network/dns-current/dns.conf"));
        assert!(rendered.contains("--conf-file=/etc/jiji/network/dns-current/dns.conf"));
        assert!(!rendered.contains("--conf-file=/etc/jiji/network/dns.conf"));
    }

    #[test]
    fn installed_generation_parser_rejects_paths_outside_managed_roots() {
        let state = parse_installed_generation(
            "/etc/jiji/network/generations/abc\n/etc/jiji/network/dns-generations/abc\n",
        )
        .unwrap();
        assert_eq!(state.network_name(), "abc");
        assert!(
            parse_installed_generation("/tmp/abc\n/etc/jiji/network/dns-generations/abc\n")
                .is_err()
        );
        assert!(parse_installed_generation(
            "/etc/jiji/network/generations/abc extra\n/etc/jiji/network/dns-generations/abc\n"
        )
        .is_err());
    }

    #[test]
    fn rollback_selects_both_previous_generations_before_restarting_services() {
        let state = InstalledGeneration {
            network: Some("/etc/jiji/network/generations/old".to_string()),
            dns: Some("/etc/jiji/network/dns-generations/old".to_string()),
        };
        let command = render_rollback_command(&state);
        let network_switch = command
            .find("ln -sfn /etc/jiji/network/generations/old")
            .unwrap();
        let dns_switch = command
            .find("ln -sfn /etc/jiji/network/dns-generations/old")
            .unwrap();
        let restart = command.find("systemctl restart wg-quick").unwrap();
        assert!(network_switch < restart);
        assert!(dns_switch < restart);
        assert!(command.contains(
            "ln -sfn /etc/jiji/network/current/wireguard.conf /etc/wireguard/jiji0.conf.new"
        ));
    }

    #[test]
    fn first_install_rollback_removes_only_jiji_managed_live_paths() {
        let command = render_rollback_command(&InstalledGeneration {
            network: None,
            dns: None,
        });
        assert!(command.contains("systemctl stop jiji-dns.service"));
        assert!(command.contains(
            "rm -f /etc/jiji/network/current /etc/wireguard/jiji0.conf /etc/jiji/network/restore.sh /etc/jiji/network/generation"
        ));
        assert!(command.contains("rm -f /etc/jiji/network/dns-current"));
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
        assert!(first.starts_with("plan-v3-"));

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
