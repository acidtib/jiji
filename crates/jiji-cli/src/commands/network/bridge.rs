use std::net::Ipv4Addr;

use anyhow::Context;
use jiji_config::ContainerEngine;
use jiji_network::{NetworkPlan, ServerPlan};

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

        Ok(format!(
            r#"#!/bin/sh
set -eu

{create_network}
test "$actual_subnet" = "{subnet}" || {{ echo "jiji network subnet is $actual_subnet, expected {subnet}. Stop attached containers with: {engine} ps --filter network=jiji. Remove the incompatible bridge with: {engine} network rm jiji. Then rerun jiji network setup --hosts {public_host} from the deployment machine." >&2; exit 1; }}
test "$actual_gateway" = "{gateway}" || {{ echo "jiji network gateway is $actual_gateway, expected {gateway}. Stop attached containers with: {engine} ps --filter network=jiji. Remove the incompatible bridge with: {engine} network rm jiji. Then rerun jiji network setup --hosts {public_host} from the deployment machine." >&2; exit 1; }}
{engine_option_validation}

ip address replace {dns_address}/32 dev jiji
sysctl -w net.ipv4.ip_forward=1 >/dev/null

ensure_rule() {{
  if ! iptables -C "$@" 2>/dev/null; then
    iptables -I "$@"
  fi
}}

ensure_rule FORWARD -i jiji -o jiji0 -s {subnet} -d {container_cidr} -j ACCEPT
ensure_rule FORWARD -i jiji0 -o jiji -s {container_cidr} -d {subnet} -j ACCEPT
if iptables -n -L DOCKER-USER >/dev/null 2>&1; then
  ensure_rule DOCKER-USER -i jiji -o jiji0 -s {subnet} -d {container_cidr} -j ACCEPT
  ensure_rule DOCKER-USER -i jiji0 -o jiji -s {container_cidr} -d {subnet} -j ACCEPT
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
        let inspect = match self.engine {
            ContainerEngine::Docker => {
                "if ! docker network inspect jiji >/dev/null 2>&1; then exit 0; fi; \
                 actual_subnet=$(docker network inspect jiji --format '{{(index .IPAM.Config 0).Subnet}}'); \
                 actual_gateway=$(docker network inspect jiji --format '{{(index .IPAM.Config 0).Gateway}}'); \
                 actual_gateway_mode=$(docker network inspect jiji --format '{{index .Options \"com.docker.network.bridge.gateway_mode_ipv4\"}}'); \
                 actual_trusted_interfaces=$(docker network inspect jiji --format '{{index .Options \"com.docker.network.bridge.trusted_host_interfaces\"}}')"
            }
            ContainerEngine::Podman => {
                "if ! podman network inspect jiji >/dev/null 2>&1; then exit 0; fi; \
                 actual_subnet=$(podman network inspect jiji --format '{{(index .Subnets 0).Subnet}}'); \
                 actual_gateway=$(podman network inspect jiji --format '{{(index .Subnets 0).Gateway}}')"
            }
        };
        format!(
            "set -eu; {inspect}; \
             test \"$actual_subnet\" = \"{subnet}\" || {{ echo \"Existing jiji bridge subnet is $actual_subnet, expected {subnet}. Stop the containers listed by: {engine} ps --filter network=jiji. Remove it with: {engine} network rm jiji. Then retry network setup.\" >&2; exit 1; }}; \
             test \"$actual_gateway\" = \"{gateway}\" || {{ echo \"Existing jiji bridge gateway is $actual_gateway, expected {gateway}. Stop the containers listed by: {engine} ps --filter network=jiji. Remove it with: {engine} network rm jiji. Then retry network setup.\" >&2; exit 1; }}; \
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
        format!(
            "[Unit]\nDescription=Restore jiji private container network\nAfter=network-online.target wg-quick@jiji0.service {engine_dependency}\nRequires=wg-quick@jiji0.service\n\n[Service]\nType=oneshot\nExecStart=/etc/jiji/network/restore.sh\nRemainAfterExit=yes\n\n[Install]\nWantedBy=multi-user.target\n"
        )
    }

    fn render_docker_network(&self) -> String {
        format!(
            "if ! docker network inspect jiji >/dev/null 2>&1; then\n  docker network create --driver bridge --subnet {} --gateway {} --opt com.docker.network.bridge.name=jiji --opt com.docker.network.bridge.enable_ip_masquerade=false --opt com.docker.network.bridge.gateway_mode_ipv4=routed --opt com.docker.network.bridge.trusted_host_interfaces=jiji0 jiji >/dev/null\nfi\nactual_subnet=$(docker network inspect jiji --format '{{{{(index .IPAM.Config 0).Subnet}}}}')\nactual_gateway=$(docker network inspect jiji --format '{{{{(index .IPAM.Config 0).Gateway}}}}')\nactual_gateway_mode=$(docker network inspect jiji --format '{{{{index .Options \"com.docker.network.bridge.gateway_mode_ipv4\"}}}}')\nactual_trusted_interfaces=$(docker network inspect jiji --format '{{{{index .Options \"com.docker.network.bridge.trusted_host_interfaces\"}}}}')",
            self.server.container_subnet, self.server.bridge_gateway
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
        format!(
            "if ! podman network inspect jiji >/dev/null 2>&1; then\n  podman network create --subnet {subnet} --gateway {gateway} --interface-name jiji jiji >/dev/null\nfi\n\
             test -x /usr/bin/busybox || {{ echo 'Podman networking requires static BusyBox at /usr/bin/busybox. Install the busybox-static package and rerun jiji network setup.' >&2; exit 1; }}\n\
             install -d -m 0755 /opt/jiji/podman-network-anchor/bin\n\
             install -m 0755 /usr/bin/busybox /opt/jiji/podman-network-anchor/bin/busybox\n\
             if ! podman container exists jiji-network-anchor; then\n\
               podman create --name jiji-network-anchor --network jiji --ip {anchor_address} --restart unless-stopped --rootfs /opt/jiji/podman-network-anchor /bin/busybox sleep 2147483647 >/dev/null\n\
             fi\n\
             test \"$(podman inspect jiji-network-anchor --format '{{{{.State.Running}}}}')\" = true || podman start jiji-network-anchor >/dev/null\n\
             actual_anchor=$(podman inspect jiji-network-anchor --format '{{{{.NetworkSettings.Networks.jiji.IPAddress}}}}')\n\
             test \"$actual_anchor\" = \"{anchor_address}\" || {{ echo \"Podman container jiji-network-anchor uses $actual_anchor, expected {anchor_address}. Remove it with: podman rm -f jiji-network-anchor. Then rerun jiji network setup.\" >&2; exit 1; }}\n\
             actual_subnet=$(podman network inspect jiji --format '{{{{(index .Subnets 0).Subnet}}}}')\n\
             actual_gateway=$(podman network inspect jiji --format '{{{{(index .Subnets 0).Gateway}}}}')",
            subnet = self.server.container_subnet,
            gateway = self.server.bridge_gateway,
            anchor_address = anchor_address,
        )
    }

    fn engine_option_validation(&self) -> &'static str {
        match self.engine {
            ContainerEngine::Docker => {
                "test \"$actual_gateway_mode\" = \"routed\" || { echo \"jiji Docker network is not in routed gateway mode. Stop attached containers with: docker ps --filter network=jiji. Remove the incompatible bridge with: docker network rm jiji. Then rerun jiji network setup from the deployment machine.\" >&2; exit 1; }\ntest \"$actual_trusted_interfaces\" = \"jiji0\" || { echo \"jiji Docker network does not trust jiji0. Stop attached containers with: docker ps --filter network=jiji. Remove the incompatible bridge with: docker network rm jiji. Then rerun jiji network setup from the deployment machine.\" >&2; exit 1; }"
            }
            ContainerEngine::Podman => "",
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
  web: { hosts: [app, data] }
"#,
        )
        .unwrap();
        NetworkPlanner::new().plan(&config).unwrap()
    }

    #[test]
    fn docker_script_uses_routed_bridge_and_strict_drift_checks() {
        let plan = plan();
        let rendered = BridgeProvisioner::new(ContainerEngine::Docker, &plan, &plan.servers["app"])
            .render_restore_script()
            .unwrap();

        assert!(rendered.contains("enable_ip_masquerade=false"));
        assert!(rendered.contains("gateway_mode_ipv4=routed"));
        assert!(rendered.contains("trusted_host_interfaces=jiji0"));
        assert!(rendered.contains("actual_subnet"));
        assert!(rendered.contains("actual_gateway"));
        assert!(rendered.contains("docker ps --filter network=jiji"));
        assert!(rendered.contains("docker network rm jiji"));
        assert!(!rendered.contains("docker network rm jiji >/dev/null"));
    }

    #[test]
    fn podman_script_uses_planned_subnet_gateway_and_interface() {
        let plan = plan();
        let rendered =
            BridgeProvisioner::new(ContainerEngine::Podman, &plan, &plan.servers["data"])
                .render_restore_script()
                .unwrap();

        assert!(rendered.contains("podman network inspect jiji"));
        assert!(rendered.contains("podman network create"));
        assert!(rendered.contains("--interface-name jiji"));
        assert!(rendered.contains("--name jiji-network-anchor"));
        assert!(rendered.contains("--rootfs /opt/jiji/podman-network-anchor"));
        assert!(rendered.contains("/usr/bin/busybox"));
        assert!(rendered.contains(&plan.servers["data"].container_subnet.to_string()));
        assert!(rendered.contains(&plan.servers["data"].bridge_gateway.to_string()));
        assert!(!rendered.contains("gateway_mode_ipv4"));
    }

    #[test]
    fn restore_unit_orders_docker_after_engine_and_wireguard() {
        let plan = plan();
        let rendered = BridgeProvisioner::new(ContainerEngine::Docker, &plan, &plan.servers["app"])
            .render_systemd_unit();
        assert!(
            rendered.contains("After=network-online.target wg-quick@jiji0.service docker.service")
        );
        assert!(rendered.contains("Requires=wg-quick@jiji0.service"));
        assert!(rendered.contains("RemainAfterExit=yes"));
    }

    #[test]
    fn existing_bridge_validation_never_removes_the_network() {
        let plan = plan();
        let rendered = BridgeProvisioner::new(ContainerEngine::Docker, &plan, &plan.servers["app"])
            .render_existing_validation_command();
        assert!(rendered.contains("docker network inspect jiji"));
        assert!(rendered.contains("docker ps --filter network=jiji"));
        assert!(!rendered.contains("docker network rm jiji;"));
        assert!(!rendered.contains("docker network rm jiji >/dev/null"));
    }
}
