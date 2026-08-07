use std::net::Ipv4Addr;

use anyhow::Context;
use jiji_config::ContainerEngine;
use jiji_network::{Ipv4Cidr, NetworkPlan, ServerPlan};
use jiji_ssh::SshSession;

/// Netavark reconfigures the kernel bridge when a Podman container is attached and can remove
/// dnsmasq's secondary `/32` address. Re-applying it is idempotent and lets Aardvark forward the
/// container's custom `--dns` queries to Jiji's project-scoped dnsmasq instance.
pub async fn reconcile_podman_dns_address(
    session: &SshSession,
    engine: ContainerEngine,
    bridge_interface: &str,
    dns_address: Ipv4Addr,
) -> anyhow::Result<()> {
    let Some(command) = render_podman_dns_address_command(engine, bridge_interface, dns_address)
    else {
        return Ok(());
    };
    let result = session.execute(&command).await?;
    require_success(session, &command, &result)
}

pub(crate) fn render_podman_dns_address_command(
    engine: ContainerEngine,
    bridge_interface: &str,
    dns_address: Ipv4Addr,
) -> Option<String> {
    (engine == ContainerEngine::Podman)
        .then(|| format!("ip address replace {dns_address}/32 dev {bridge_interface}"))
}

pub struct BridgeProvisioner<'a> {
    engine: ContainerEngine,
    plan: &'a NetworkPlan,
    server: &'a ServerPlan,
}

#[derive(Debug)]
pub struct BridgeMigration {
    old_subnet: Ipv4Cidr,
    old_gateway: Ipv4Addr,
    attachments: Vec<(String, Ipv4Addr)>,
}

impl BridgeMigration {
    pub fn includes_proxy(&self) -> bool {
        self.attachments
            .iter()
            .any(|(name, _)| name == jiji_network::CONTAINER_NAME)
    }

    pub fn previous_proxy_address(&self) -> Option<Ipv4Addr> {
        self.attachments
            .iter()
            .find_map(|(name, address)| (name == jiji_network::CONTAINER_NAME).then_some(*address))
    }
}

impl<'a> BridgeProvisioner<'a> {
    pub fn new(engine: ContainerEngine, plan: &'a NetworkPlan, server: &'a ServerPlan) -> Self {
        Self {
            engine,
            plan,
            server,
        }
    }

    pub fn network_inspection_command(engine: ContainerEngine) -> &'static str {
        match engine {
            ContainerEngine::Docker => {
                "if command -v docker >/dev/null 2>&1; then docker network inspect $(docker network ls -q) --format 'NETWORK {{.Name}} {{range .IPAM.Config}}{{.Subnet}} {{end}}' 2>/dev/null || true; fi"
            }
            ContainerEngine::Podman => {
                "if command -v podman >/dev/null 2>&1; then podman network inspect --format 'NETWORK {{.Name}} {{range .Subnets}}{{.Subnet}} {{end}}' $(podman network ls -q) 2>/dev/null || true; fi"
            }
        }
    }

    /// Peer endpoint hosts, parsed as the public IPv4 addresses the rendered script's per-peer
    /// WireGuard `INPUT` firewall rules need. Shared with `jiji-agent`'s `LocalRuntimeConfig`,
    /// which carries the same parsed list so the agent's native bring-up (Phase 9) never has to
    /// duplicate this parsing.
    pub fn peer_public_ips(&self) -> anyhow::Result<Vec<Ipv4Addr>> {
        self.server
            .peers
            .iter()
            .map(|peer| {
                let host = peer
                    .endpoint
                    .rsplit_once(':')
                    .map(|(host, _)| host)
                    .unwrap_or(&peer.endpoint);
                host.parse::<Ipv4Addr>()
            })
            .collect::<Result<Vec<_>, _>>()
            .context("WireGuard firewall rules require server hosts to be public IPv4 addresses")
    }

    fn script_params<'p>(
        &'p self,
        peer_public_ips: &'p [Ipv4Addr],
    ) -> jiji_network::BridgeScriptParams<'p> {
        jiji_network::BridgeScriptParams {
            bridge_name: &self.server.bridge_name,
            bridge_interface: &self.server.bridge_interface,
            wireguard_interface: &self.server.wireguard_interface,
            container_subnet: self.server.container_subnet,
            bridge_gateway: self.server.bridge_gateway,
            dns_address: self.server.dns_address,
            container_cidr: self.plan.container_cidr,
            wireguard_port: self.server.wireguard_port,
            peer_public_ips,
            public_host: &self.server.public_host,
        }
    }

    fn engine_kind(&self) -> jiji_network::BridgeEngineKind {
        match self.engine {
            ContainerEngine::Docker => jiji_network::BridgeEngineKind::Docker,
            ContainerEngine::Podman => jiji_network::BridgeEngineKind::Podman,
        }
    }

    pub fn render_restore_script(&self) -> anyhow::Result<String> {
        let peer_public_ips = self.peer_public_ips()?;
        let params = self.script_params(&peer_public_ips);
        Ok(jiji_network::render_restore_script(
            self.engine_kind(),
            &params,
        ))
    }

    pub fn render_existing_validation_command(&self) -> String {
        let params = self.script_params(&[]);
        jiji_network::render_existing_validation_command(self.engine_kind(), &params)
    }

    pub async fn inspect_migration(
        &self,
        session: &SshSession,
    ) -> anyhow::Result<Option<BridgeMigration>> {
        let bridge = &self.server.bridge_name;
        let inspect = match self.engine {
            ContainerEngine::Docker => format!(
                "if ! docker network inspect {bridge} >/dev/null 2>&1; then printf '%s\\n' MISSING; exit 0; fi; \
                 subnet=$(docker network inspect {bridge} --format '{{{{(index .IPAM.Config 0).Subnet}}}}'); \
                 gateway=$(docker network inspect {bridge} --format '{{{{(index .IPAM.Config 0).Gateway}}}}'); \
                 printf '%s|%s\\n' \"$subnet\" \"$gateway\""
            ),
            ContainerEngine::Podman => format!(
                "if ! podman network inspect {bridge} >/dev/null 2>&1; then printf '%s\\n' MISSING; exit 0; fi; \
                 subnet=$(podman network inspect {bridge} --format '{{{{(index .Subnets 0).Subnet}}}}'); \
                 gateway=$(podman network inspect {bridge} --format '{{{{(index .Subnets 0).Gateway}}}}'); \
                 printf '%s|%s\\n' \"$subnet\" \"$gateway\""
            ),
        };
        let result = session.execute(&format!("set -eu; {inspect}")).await?;
        require_success(session, &inspect, &result)?;
        let value = result.stdout.trim();
        if value == "MISSING" || value.is_empty() {
            return Ok(None);
        }
        let (subnet, gateway) = value
            .split_once('|')
            .ok_or_else(|| anyhow::anyhow!("Invalid bridge inspection response '{value}'"))?;
        let old_subnet = subnet.parse::<Ipv4Cidr>()?;
        let old_gateway = gateway.parse::<Ipv4Addr>()?;
        if old_subnet == self.server.container_subnet && old_gateway == self.server.bridge_gateway {
            return Ok(None);
        }

        let list = format!(
            "{} ps -a --filter network={bridge} --format '{{{{.Names}}}}'",
            self.engine
        );
        let result = session.execute(&list).await?;
        require_success(session, &list, &result)?;
        let mut attachments = Vec::new();
        for name in result
            .stdout
            .lines()
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            if name != jiji_network::CONTAINER_NAME {
                anyhow::bail!(
                    "Cannot change bridge CIDR while service container '{name}' is attached on {}. Dynamic deployment addresses are durable catalog leases; remove the affected service deployment, rerun `jiji network setup`, then deploy it again.",
                    session.host()
                );
            }
            let address_command = format!(
                "{} inspect {name} --format '{{{{(index .NetworkSettings.Networks \"{bridge}\").IPAddress}}}}'",
                self.engine
            );
            let result = session.execute(&address_command).await?;
            require_success(session, &address_command, &result)?;
            let old_address = result.stdout.trim().parse::<Ipv4Addr>().with_context(|| {
                format!("Container '{name}' returned an invalid address during network migration")
            })?;
            attachments.push((name.to_string(), old_address));
        }
        Ok(Some(BridgeMigration {
            old_subnet,
            old_gateway,
            attachments,
        }))
    }

    pub async fn detach_for_migration(
        &self,
        session: &SshSession,
        migration: &BridgeMigration,
    ) -> anyhow::Result<()> {
        let bridge = &self.server.bridge_name;
        let mut commands = Vec::new();
        commands.extend(migration.attachments.iter().map(|(name, _)| {
            format!(
                "{} network disconnect -f {bridge} {name} >/dev/null",
                self.engine
            )
        }));
        commands.push(format!("{} network rm {bridge} >/dev/null", self.engine));
        if self.engine == ContainerEngine::Podman {
            commands.push(format!(
                "ip link delete {} type bridge 2>/dev/null || true",
                self.server.bridge_interface
            ));
        }
        let command = format!("set -eu; {}", commands.join("; "));
        let result = session.execute(&command).await?;
        require_success(session, &command, &result)
    }

    pub async fn reattach_after_migration(
        &self,
        session: &SshSession,
        migration: &BridgeMigration,
    ) -> anyhow::Result<bool> {
        let addresses = self.planned_attachment_addresses();
        let bridge = &self.server.bridge_name;
        let mut proxy_reattached = false;
        for (name, _) in &migration.attachments {
            let address = addresses[name];
            let command = format!(
                "{} network connect --ip {address} {bridge} {name}",
                self.engine
            );
            let result = session.execute(&command).await?;
            require_success(session, &command, &result)?;
            proxy_reattached |= name == jiji_network::CONTAINER_NAME;
        }
        reconcile_podman_dns_address(
            session,
            self.engine,
            &self.server.bridge_interface,
            self.server.dns_address,
        )
        .await?;
        Ok(proxy_reattached)
    }

    pub async fn restore_previous_bridge(
        &self,
        session: &SshSession,
        migration: &BridgeMigration,
    ) -> anyhow::Result<()> {
        let bridge = &self.server.bridge_name;
        let mut commands = migration
            .attachments
            .iter()
            .map(|(name, _)| {
                format!(
                    "{} network disconnect -f {bridge} {name} >/dev/null 2>&1 || true",
                    self.engine
                )
            })
            .collect::<Vec<_>>();
        commands.push(format!(
            "{} network rm {bridge} >/dev/null 2>&1 || true",
            self.engine
        ));
        if self.engine == ContainerEngine::Podman {
            commands.push(format!(
                "ip link delete {} type bridge 2>/dev/null || true",
                self.server.bridge_interface
            ));
        }
        commands.push(self.render_network_create(migration.old_subnet, migration.old_gateway));
        commands.extend(migration.attachments.iter().map(|(name, address)| {
            format!(
                "{} network connect --ip {address} {bridge} {name}",
                self.engine
            )
        }));
        let command = format!("set -eu; {}", commands.join("; "));
        let result = session.execute(&command).await?;
        require_success(session, &command, &result)
    }

    fn planned_attachment_addresses(&self) -> std::collections::BTreeMap<String, Ipv4Addr> {
        let mut addresses = std::collections::BTreeMap::new();
        addresses.insert(
            jiji_network::CONTAINER_NAME.to_string(),
            self.server.proxy_address,
        );
        addresses
    }

    fn render_network_create(&self, subnet: Ipv4Cidr, gateway: Ipv4Addr) -> String {
        let bridge = &self.server.bridge_name;
        let interface = &self.server.bridge_interface;
        match self.engine {
            ContainerEngine::Docker => format!(
                "docker network create --driver bridge --subnet {subnet} --gateway {gateway} \
                 --opt com.docker.network.bridge.name={interface} \
                 --opt com.docker.network.bridge.enable_ip_masquerade=false \
                 --opt com.docker.network.bridge.gateway_mode_ipv4=routed \
                 --opt com.docker.network.bridge.trusted_host_interfaces={} {bridge} >/dev/null",
                self.server.wireguard_interface
            ),
            ContainerEngine::Podman => format!(
                "podman network create --subnet {subnet} --gateway {gateway} \
                 --interface-name {interface} {bridge} >/dev/null"
            ),
        }
    }
}

fn require_success(
    session: &SshSession,
    command: &str,
    result: &jiji_ssh::CommandResult,
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

#[cfg(test)]
mod tests {
    use super::*;
    use jiji_config::Config;
    use jiji_network::NetworkPlanner;

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
"#,
        )
        .unwrap();
        NetworkPlanner::new().plan(&config).unwrap()
    }

    #[test]
    fn docker_script_uses_routed_bridge_and_strict_drift_checks() {
        let plan = plan();
        let server = &plan.servers["app"];
        let rendered = BridgeProvisioner::new(ContainerEngine::Docker, &plan, server)
            .render_restore_script()
            .unwrap();

        assert!(rendered.contains("enable_ip_masquerade=false"));
        assert!(rendered.contains("gateway_mode_ipv4=routed"));
        assert!(rendered.contains(&format!(
            "trusted_host_interfaces={}",
            server.wireguard_interface
        )));
        assert!(rendered.contains("actual_subnet"));
        assert!(rendered.contains("actual_gateway"));
        assert!(rendered.contains(&format!(
            "docker ps --filter network={}",
            server.bridge_name
        )));
        assert!(rendered.contains(&format!("docker network rm {}", server.bridge_name)));
        assert!(!rendered.contains(&format!(
            "docker network rm {} >/dev/null",
            server.bridge_name
        )));
        // Derived names must never collide with a hardcoded literal from before this change.
        assert!(!rendered.contains("--opt com.docker.network.bridge.name=jiji "));
    }

    #[test]
    fn podman_script_uses_planned_subnet_gateway_and_interface() {
        let plan = plan();
        let server = &plan.servers["data"];
        let rendered = BridgeProvisioner::new(ContainerEngine::Podman, &plan, server)
            .render_restore_script()
            .unwrap();

        assert!(rendered.contains(&format!("podman network inspect {}", server.bridge_name)));
        assert!(rendered.contains("podman network create"));
        assert!(rendered.contains(&format!("--interface-name {}", server.bridge_interface)));
        assert!(rendered.contains(&format!(
            "ip link add name {} type bridge",
            server.bridge_interface
        )));
        assert!(rendered.contains(&format!(
            "ip address replace {}/{} dev {}",
            server.bridge_gateway,
            server.container_subnet.prefix(),
            server.bridge_interface
        )));
        assert!(rendered.contains(&server.container_subnet.to_string()));
        assert!(rendered.contains(&server.bridge_gateway.to_string()));
        assert!(!rendered.contains("gateway_mode_ipv4"));
        assert!(!rendered.contains("anchor"));
    }

    #[test]
    fn podman_reconciles_the_project_dns_address_after_network_activation() {
        let plan = plan();
        let server = &plan.servers["app"];
        assert_eq!(
            render_podman_dns_address_command(
                ContainerEngine::Podman,
                &server.bridge_interface,
                server.dns_address,
            ),
            Some(format!(
                "ip address replace {}/32 dev {}",
                server.dns_address, server.bridge_interface
            ))
        );
        assert_eq!(
            render_podman_dns_address_command(
                ContainerEngine::Docker,
                &server.bridge_interface,
                server.dns_address,
            ),
            None
        );
    }

    #[test]
    fn existing_bridge_validation_never_removes_the_network() {
        let plan = plan();
        let server = &plan.servers["app"];
        let rendered = BridgeProvisioner::new(ContainerEngine::Docker, &plan, server)
            .render_existing_validation_command();
        assert!(rendered.contains(&format!("docker network inspect {}", server.bridge_name)));
        assert!(rendered.contains(&format!(
            "docker ps --filter network={}",
            server.bridge_name
        )));
        assert!(!rendered.contains(&format!("docker network rm {};", server.bridge_name)));
        assert!(!rendered.contains(&format!(
            "docker network rm {} >/dev/null",
            server.bridge_name
        )));
    }

    #[test]
    fn migration_only_assigns_the_shared_proxy_a_fixed_address() {
        let plan = plan();
        let server = &plan.servers["app"];
        let bridge = BridgeProvisioner::new(ContainerEngine::Docker, &plan, server);
        let addresses = bridge.planned_attachment_addresses();

        assert_eq!(addresses.len(), 1);
        assert_eq!(
            addresses[jiji_network::CONTAINER_NAME],
            server.proxy_address
        );
    }

    #[test]
    fn migration_recreates_engine_networks_with_the_previous_shape_for_rollback() {
        let plan = plan();
        let server = &plan.servers["app"];
        let subnet = "100.125.0.0/21".parse().unwrap();
        let gateway = "100.125.0.1".parse().unwrap();

        let docker = BridgeProvisioner::new(ContainerEngine::Docker, &plan, server)
            .render_network_create(subnet, gateway);
        assert!(docker.contains("--subnet 100.125.0.0/21"));
        assert!(docker.contains("--gateway 100.125.0.1"));
        assert!(docker.contains("gateway_mode_ipv4=routed"));

        let podman = BridgeProvisioner::new(ContainerEngine::Podman, &plan, server)
            .render_network_create(subnet, gateway);
        assert!(podman.contains("--subnet 100.125.0.0/21"));
        assert!(podman.contains("--gateway 100.125.0.1"));
        assert!(podman.contains("--interface-name"));
    }

    #[test]
    fn two_projects_produce_distinct_bridge_names() {
        let plan_a = plan();
        let mut config_b: Config = serde_yaml::from_str(
            r#"
project: other
builder: { engine: docker }
servers:
  app: { host: 203.0.113.10 }
services: {}
"#,
        )
        .unwrap();
        config_b.project = "other".to_string();
        let plan_b = NetworkPlanner::new().plan(&config_b).unwrap();

        assert_ne!(
            plan_a.servers["app"].bridge_name,
            plan_b.servers["app"].bridge_name
        );
        assert_ne!(
            plan_a.servers["app"].bridge_interface,
            plan_b.servers["app"].bridge_interface
        );
    }
}
