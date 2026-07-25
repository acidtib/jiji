use std::net::Ipv4Addr;

use anyhow::Context;
use jiji_config::ContainerEngine;
use jiji_network::{NetworkPlan, ServerPlan};

/// Podman's project-scoped bridge-keepalive container (see `render_podman_network`), reused by
/// `crate::network_teardown` to remove it before removing this project's bridge network itself.
pub(crate) fn network_anchor_container_name(project: &str) -> String {
    format!(
        "jiji-network-anchor-{}",
        jiji_network::systemd_unit_slug(project)
    )
}

pub struct BridgeProvisioner<'a> {
    engine: ContainerEngine,
    plan: &'a NetworkPlan,
    server: &'a ServerPlan,
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
        // Offset 3. `ServerPlan::proxy_address` (offset 4) is reserved right after this one so the
        // two addresses can never collide; keep them in sync if either offset ever changes.
        let anchor_address = self
            .server
            .container_subnet
            .address(3)
            .expect("validated container subnet has an anchor address");
        let bridge_name = &self.server.bridge_name;
        let bridge_interface = &self.server.bridge_interface;
        let anchor_name = network_anchor_container_name(&self.plan.project);
        format!(
            "if ! podman network inspect {bridge_name} >/dev/null 2>&1; then\n  podman network create --subnet {subnet} --gateway {gateway} --interface-name {bridge_interface} {bridge_name} >/dev/null\nfi\n\
             test -x /usr/bin/busybox || {{ echo 'Podman networking requires static BusyBox at /usr/bin/busybox. Install the busybox-static package and rerun jiji network setup.' >&2; exit 1; }}\n\
             install -d -m 0755 /opt/jiji/podman-network-anchor/bin\n\
             install -m 0755 /usr/bin/busybox /opt/jiji/podman-network-anchor/bin/busybox\n\
             if ! podman container exists {anchor_name}; then\n\
               podman create --name {anchor_name} --network {bridge_name} --ip {anchor_address} --restart unless-stopped --rootfs /opt/jiji/podman-network-anchor /bin/busybox sleep 2147483647 >/dev/null\n\
             fi\n\
             test \"$(podman inspect {anchor_name} --format '{{{{.State.Running}}}}')\" = true || podman start {anchor_name} >/dev/null\n\
             actual_anchor=$(podman inspect {anchor_name} --format '{{{{(index .NetworkSettings.Networks \"{bridge_name}\").IPAddress}}}}')\n\
             test \"$actual_anchor\" = \"{anchor_address}\" || {{ echo \"Podman container {anchor_name} uses $actual_anchor, expected {anchor_address}. Remove it with: podman rm -f {anchor_name}. Then rerun jiji network setup.\" >&2; exit 1; }}\n\
             actual_subnet=$(podman network inspect {bridge_name} --format '{{{{(index .Subnets 0).Subnet}}}}')\n\
             actual_gateway=$(podman network inspect {bridge_name} --format '{{{{(index .Subnets 0).Gateway}}}}')",
            subnet = self.server.container_subnet,
            gateway = self.server.bridge_gateway,
            anchor_address = anchor_address,
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

        let anchor_name = network_anchor_container_name(&plan.project);
        assert!(rendered.contains(&format!("podman network inspect {}", server.bridge_name)));
        assert!(rendered.contains("podman network create"));
        assert!(rendered.contains(&format!("--interface-name {}", server.bridge_interface)));
        assert!(rendered.contains(&format!("--name {anchor_name}")));
        assert!(rendered.contains("--rootfs /opt/jiji/podman-network-anchor"));
        assert!(rendered.contains("/usr/bin/busybox"));
        assert!(rendered.contains(&server.container_subnet.to_string()));
        assert!(rendered.contains(&server.bridge_gateway.to_string()));
        assert!(!rendered.contains("gateway_mode_ipv4"));
        // Regression guard, confirmed live: Go templates' bare dot-path field access
        // (`.NetworkSettings.Networks.{name}.IPAddress`) cannot parse a hyphenated map key --
        // `bridge_name` always contains hyphens (`jiji-{slug}`), so this must use `index` with a
        // quoted key instead, chaining `.IPAddress` onto the parenthesized result.
        assert!(rendered.contains(&format!(
            "(index .NetworkSettings.Networks \"{}\").IPAddress",
            server.bridge_name
        )));
        assert!(!rendered.contains(&format!(
            ".NetworkSettings.Networks.{}.IPAddress",
            server.bridge_name
        )));
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
    fn two_projects_produce_distinct_bridge_and_anchor_names() {
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
        assert_ne!(
            network_anchor_container_name(&plan_a.project),
            network_anchor_container_name(&plan_b.project)
        );
    }
}
