use std::net::Ipv4Addr;

use anyhow::Context;
use jiji_config::ContainerEngine;
use jiji_network::{BackendSlot, Ipv4Cidr, NetworkPlan, ServerPlan};
use jiji_ssh::SshSession;

use crate::container_runtime::{backend_address, container_name};

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
            .any(|(name, _)| name == "kamal-proxy")
    }

    pub fn previous_proxy_address(&self) -> Option<Ipv4Addr> {
        self.attachments
            .iter()
            .find_map(|(name, address)| (name == "kamal-proxy").then_some(*address))
    }

    pub fn previous_container_address(&self, name: &str) -> Option<Ipv4Addr> {
        self.attachments
            .iter()
            .find_map(|(attached, address)| (attached == name).then_some(*address))
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

    pub fn render_restore_script(&self) -> anyhow::Result<String> {
        let create_network = match self.engine {
            ContainerEngine::Docker => self.render_docker_network(),
            ContainerEngine::Podman => self.render_podman_network(),
        };
        let peer_input_rules = self
            .server
            .peers
            .iter()
            .map(|peer| {
                let host = peer
                    .endpoint
                    .rsplit_once(':')
                    .map(|(host, _)| host)
                    .unwrap_or(&peer.endpoint);
                host.parse::<Ipv4Addr>().map(|address| {
                    format!(
                        "ensure_rule INPUT -p udp -s {address}/32 --dport {} -j ACCEPT",
                        self.server.wireguard_port
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .context("WireGuard firewall rules require server hosts to be public IPv4 addresses")?
            .join("\n");

        let bridge_name = &self.server.bridge_name;
        let bridge_interface = &self.server.bridge_interface;
        let wireguard_interface = &self.server.wireguard_interface;
        Ok(format!(
            r#"#!/bin/sh
set -eu

{create_network}
test "$actual_subnet" = "{subnet}" || {{ echo "{bridge_name} network subnet is $actual_subnet, expected {subnet}. Stop attached containers with: {engine} ps --filter network={bridge_name}. Remove the incompatible bridge with: {engine} network rm {bridge_name}. Then rerun jiji network setup --hosts {public_host} from the deployment machine." >&2; exit 1; }}
test "$actual_gateway" = "{gateway}" || {{ echo "{bridge_name} network gateway is $actual_gateway, expected {gateway}. Stop attached containers with: {engine} ps --filter network={bridge_name}. Remove the incompatible bridge with: {engine} network rm {bridge_name}. Then rerun jiji network setup --hosts {public_host} from the deployment machine." >&2; exit 1; }}
{engine_option_validation}

ip address replace {dns_address}/32 dev {bridge_interface}
sysctl -w net.ipv4.ip_forward=1 >/dev/null

ensure_rule() {{
  if ! iptables -C "$@" 2>/dev/null; then
    iptables -I "$@"
  fi
}}

ensure_rule FORWARD -i {bridge_interface} -o {wireguard_interface} -s {subnet} -d {container_cidr} -j ACCEPT
ensure_rule FORWARD -i {wireguard_interface} -o {bridge_interface} -s {container_cidr} -d {subnet} -j ACCEPT
if iptables -n -L DOCKER-USER >/dev/null 2>&1; then
  ensure_rule DOCKER-USER -i {bridge_interface} -o {wireguard_interface} -s {subnet} -d {container_cidr} -j ACCEPT
  ensure_rule DOCKER-USER -i {wireguard_interface} -o {bridge_interface} -s {container_cidr} -d {subnet} -j ACCEPT
fi
{peer_input_rules}
"#,
            subnet = self.server.container_subnet,
            gateway = self.server.bridge_gateway,
            dns_address = self.server.dns_address,
            container_cidr = self.plan.container_cidr,
            engine = self.engine,
            public_host = self.server.public_host,
            engine_option_validation = self.engine_option_validation(),
        ))
    }

    pub fn render_existing_validation_command(&self) -> String {
        let bridge_name = &self.server.bridge_name;
        let inspect = match self.engine {
            ContainerEngine::Docker => {
                format!(
                    "if ! docker network inspect {bridge_name} >/dev/null 2>&1; then exit 0; fi; \
                     actual_subnet=$(docker network inspect {bridge_name} --format '{{{{(index .IPAM.Config 0).Subnet}}}}'); \
                     actual_gateway=$(docker network inspect {bridge_name} --format '{{{{(index .IPAM.Config 0).Gateway}}}}'); \
                     actual_gateway_mode=$(docker network inspect {bridge_name} --format '{{{{index .Options \"com.docker.network.bridge.gateway_mode_ipv4\"}}}}'); \
                     actual_trusted_interfaces=$(docker network inspect {bridge_name} --format '{{{{index .Options \"com.docker.network.bridge.trusted_host_interfaces\"}}}}')"
                )
            }
            ContainerEngine::Podman => {
                format!(
                    "if ! podman network inspect {bridge_name} >/dev/null 2>&1; then exit 0; fi; \
                     actual_subnet=$(podman network inspect {bridge_name} --format '{{{{(index .Subnets 0).Subnet}}}}'); \
                     actual_gateway=$(podman network inspect {bridge_name} --format '{{{{(index .Subnets 0).Gateway}}}}')"
                )
            }
        };
        format!(
            "set -eu; {inspect}; \
             test \"$actual_subnet\" = \"{subnet}\" || {{ echo \"Existing {bridge_name} bridge subnet is $actual_subnet, expected {subnet}. Stop the containers listed by: {engine} ps --filter network={bridge_name}. Remove it with: {engine} network rm {bridge_name}. Then retry network setup.\" >&2; exit 1; }}; \
             test \"$actual_gateway\" = \"{gateway}\" || {{ echo \"Existing {bridge_name} bridge gateway is $actual_gateway, expected {gateway}. Stop the containers listed by: {engine} ps --filter network={bridge_name}. Remove it with: {engine} network rm {bridge_name}. Then retry network setup.\" >&2; exit 1; }}; \
             {engine_options}",
            subnet = self.server.container_subnet,
            gateway = self.server.bridge_gateway,
            engine = self.engine,
            engine_options = self.engine_option_validation(),
        )
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
        let expected = self.planned_attachment_addresses();
        let mut attachments = Vec::new();
        for name in result
            .stdout
            .lines()
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            if !expected.contains_key(name) {
                anyhow::bail!(
                    "Cannot migrate bridge '{bridge}' on {} while unknown container '{name}' is attached. Remove or disconnect that container, then retry `jiji network setup`.",
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
            proxy_reattached |= name == "kamal-proxy";
        }
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
        for endpoint in self
            .plan
            .endpoints
            .values()
            .filter(|endpoint| endpoint.server == self.server.name)
        {
            addresses.insert(
                container_name(&self.plan.project, &endpoint.service, BackendSlot::A),
                backend_address(endpoint, BackendSlot::A),
            );
            addresses.insert(
                container_name(&self.plan.project, &endpoint.service, BackendSlot::B),
                backend_address(endpoint, BackendSlot::B),
            );
        }
        addresses.insert("kamal-proxy".to_string(), self.server.proxy_address);
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

    pub fn render_systemd_unit(&self) -> String {
        let engine_dependency = match self.engine {
            ContainerEngine::Docker => "docker.service",
            ContainerEngine::Podman => "network-online.target",
        };
        let wireguard_interface = &self.server.wireguard_interface;
        let slug = jiji_network::systemd_unit_slug(&self.plan.project);
        let network_dir = crate::commands::network::setup::network_dir(&slug);
        format!(
            "[Unit]\nDescription=Restore jiji private container network\nAfter=network-online.target wg-quick@{wireguard_interface}.service {engine_dependency}\nRequires=wg-quick@{wireguard_interface}.service\n\n[Service]\nType=oneshot\nExecStart={network_dir}/restore.sh\nRemainAfterExit=yes\n\n[Install]\nWantedBy=multi-user.target\n"
        )
    }

    fn render_docker_network(&self) -> String {
        let bridge_name = &self.server.bridge_name;
        let bridge_interface = &self.server.bridge_interface;
        let wireguard_interface = &self.server.wireguard_interface;
        format!(
            "if ! docker network inspect {bridge_name} >/dev/null 2>&1; then\n  docker network create --driver bridge --subnet {subnet} --gateway {gateway} --opt com.docker.network.bridge.name={bridge_interface} --opt com.docker.network.bridge.enable_ip_masquerade=false --opt com.docker.network.bridge.gateway_mode_ipv4=routed --opt com.docker.network.bridge.trusted_host_interfaces={wireguard_interface} {bridge_name} >/dev/null\nfi\nactual_subnet=$(docker network inspect {bridge_name} --format '{{{{(index .IPAM.Config 0).Subnet}}}}')\nactual_gateway=$(docker network inspect {bridge_name} --format '{{{{(index .IPAM.Config 0).Gateway}}}}')\nactual_gateway_mode=$(docker network inspect {bridge_name} --format '{{{{index .Options \"com.docker.network.bridge.gateway_mode_ipv4\"}}}}')\nactual_trusted_interfaces=$(docker network inspect {bridge_name} --format '{{{{index .Options \"com.docker.network.bridge.trusted_host_interfaces\"}}}}')",
            subnet = self.server.container_subnet,
            gateway = self.server.bridge_gateway,
        )
    }

    fn render_podman_network(&self) -> String {
        let bridge_name = &self.server.bridge_name;
        let bridge_interface = &self.server.bridge_interface;
        format!(
            "if ! podman network inspect {bridge_name} >/dev/null 2>&1; then\n  podman network create --subnet {subnet} --gateway {gateway} --interface-name {bridge_interface} {bridge_name} >/dev/null\nfi\n\
             if ! ip link show {bridge_interface} >/dev/null 2>&1; then\n\
               ip link add name {bridge_interface} type bridge\n\
             fi\n\
             ip link set {bridge_interface} up\n\
             ip address replace {gateway}/{prefix} dev {bridge_interface}\n\
             actual_subnet=$(podman network inspect {bridge_name} --format '{{{{(index .Subnets 0).Subnet}}}}')\n\
             actual_gateway=$(podman network inspect {bridge_name} --format '{{{{(index .Subnets 0).Gateway}}}}')",
            subnet = self.server.container_subnet,
            gateway = self.server.bridge_gateway,
            prefix = self.server.container_subnet.prefix(),
        )
    }

    fn engine_option_validation(&self) -> String {
        let bridge_name = &self.server.bridge_name;
        let wireguard_interface = &self.server.wireguard_interface;
        match self.engine {
            ContainerEngine::Docker => {
                format!(
                    "test \"$actual_gateway_mode\" = \"routed\" || {{ echo \"{bridge_name} Docker network is not in routed gateway mode. Stop attached containers with: docker ps --filter network={bridge_name}. Remove the incompatible bridge with: docker network rm {bridge_name}. Then rerun jiji network setup from the deployment machine.\" >&2; exit 1; }}\ntest \"$actual_trusted_interfaces\" = \"{wireguard_interface}\" || {{ echo \"{bridge_name} Docker network does not trust {wireguard_interface}. Stop attached containers with: docker ps --filter network={bridge_name}. Remove the incompatible bridge with: docker network rm {bridge_name}. Then rerun jiji network setup from the deployment machine.\" >&2; exit 1; }}"
                )
            }
            ContainerEngine::Podman => String::new(),
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
    fn restore_unit_orders_docker_after_engine_and_wireguard() {
        let plan = plan();
        let server = &plan.servers["app"];
        let rendered =
            BridgeProvisioner::new(ContainerEngine::Docker, &plan, server).render_systemd_unit();
        assert!(rendered.contains(&format!(
            "After=network-online.target wg-quick@{}.service docker.service",
            server.wireguard_interface
        )));
        assert!(rendered.contains(&format!(
            "Requires=wg-quick@{}.service",
            server.wireguard_interface
        )));
        assert!(rendered.contains("RemainAfterExit=yes"));
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
    fn migration_maps_each_service_slot_and_proxy_to_the_new_plan() {
        let plan = plan();
        let server = &plan.servers["app"];
        let bridge = BridgeProvisioner::new(ContainerEngine::Docker, &plan, server);
        let addresses = bridge.planned_attachment_addresses();
        let endpoint = &plan.endpoints["demo:web:app"];

        assert_eq!(
            addresses["demo-web-a"],
            backend_address(endpoint, BackendSlot::A)
        );
        assert_eq!(
            addresses["demo-web-b"],
            backend_address(endpoint, BackendSlot::B)
        );
        assert_eq!(addresses["kamal-proxy"], server.proxy_address);
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
