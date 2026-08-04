//! Pure rendering of the shell script that brings up a project's bridge network, binds its DNS
//! address, and installs the iptables rules routing traffic between the bridge and the WireGuard
//! mesh. Shared by `jiji-cli` (which still renders it for the migration/drift-validation paths in
//! `commands/network/bridge.rs`) and `jiji-agent` (which runs it directly at startup/reconcile
//! time). Kept dependency-free of both `jiji-cli` and `jiji-agent` so neither has to depend on the
//! other to share this logic -- a duplicated re-implementation on either side would silently
//! drift from the other, the same "two authorities for one piece of state" risk documented
//! elsewhere in this codebase's history.

use std::fmt;
use std::net::Ipv4Addr;

use crate::Ipv4Cidr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeEngineKind {
    Docker,
    Podman,
}

impl fmt::Display for BridgeEngineKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            BridgeEngineKind::Docker => "docker",
            BridgeEngineKind::Podman => "podman",
        })
    }
}

/// Every value the rendered script needs, already resolved to primitives so this module never
/// has to know about `NetworkPlan`/`ServerPlan` (jiji-cli's types) or `MeshConfig` (jiji-agent's).
/// `peer_public_ips` is pre-parsed by the caller (each server's WireGuard peer endpoint host,
/// which must already be validated as a public IPv4 address) rather than parsed here, so this
/// module stays infallible.
pub struct BridgeScriptParams<'a> {
    pub bridge_name: &'a str,
    pub bridge_interface: &'a str,
    pub wireguard_interface: &'a str,
    pub container_subnet: Ipv4Cidr,
    pub bridge_gateway: Ipv4Addr,
    pub dns_address: Ipv4Addr,
    pub container_cidr: Ipv4Cidr,
    pub wireguard_port: u16,
    pub peer_public_ips: &'a [Ipv4Addr],
    /// Used only inside actionable error messages the script prints on drift.
    pub public_host: &'a str,
}

pub fn render_restore_script(engine: BridgeEngineKind, params: &BridgeScriptParams<'_>) -> String {
    let create_network = match engine {
        BridgeEngineKind::Docker => render_docker_network(params),
        BridgeEngineKind::Podman => render_podman_network(params),
    };
    let peer_input_rules = params
        .peer_public_ips
        .iter()
        .map(|address| {
            format!(
                "ensure_rule INPUT -p udp -s {address}/32 --dport {} -j ACCEPT",
                params.wireguard_port
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let bridge_name = params.bridge_name;
    let bridge_interface = params.bridge_interface;
    let wireguard_interface = params.wireguard_interface;
    format!(
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
        subnet = params.container_subnet,
        gateway = params.bridge_gateway,
        dns_address = params.dns_address,
        container_cidr = params.container_cidr,
        engine = engine,
        public_host = params.public_host,
        engine_option_validation = engine_option_validation(engine, params),
    )
}

pub fn render_existing_validation_command(
    engine: BridgeEngineKind,
    params: &BridgeScriptParams<'_>,
) -> String {
    let bridge_name = params.bridge_name;
    let inspect = match engine {
        BridgeEngineKind::Docker => {
            format!(
                "if ! docker network inspect {bridge_name} >/dev/null 2>&1; then exit 0; fi; \
                 actual_subnet=$(docker network inspect {bridge_name} --format '{{{{(index .IPAM.Config 0).Subnet}}}}'); \
                 actual_gateway=$(docker network inspect {bridge_name} --format '{{{{(index .IPAM.Config 0).Gateway}}}}'); \
                 actual_gateway_mode=$(docker network inspect {bridge_name} --format '{{{{index .Options \"com.docker.network.bridge.gateway_mode_ipv4\"}}}}'); \
                 actual_trusted_interfaces=$(docker network inspect {bridge_name} --format '{{{{index .Options \"com.docker.network.bridge.trusted_host_interfaces\"}}}}')"
            )
        }
        BridgeEngineKind::Podman => {
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
        subnet = params.container_subnet,
        gateway = params.bridge_gateway,
        engine = engine,
        engine_options = engine_option_validation(engine, params),
    )
}

fn render_docker_network(params: &BridgeScriptParams<'_>) -> String {
    let bridge_name = params.bridge_name;
    let bridge_interface = params.bridge_interface;
    let wireguard_interface = params.wireguard_interface;
    format!(
        "if ! docker network inspect {bridge_name} >/dev/null 2>&1; then\n  docker network create --driver bridge --subnet {subnet} --gateway {gateway} --opt com.docker.network.bridge.name={bridge_interface} --opt com.docker.network.bridge.enable_ip_masquerade=false --opt com.docker.network.bridge.gateway_mode_ipv4=routed --opt com.docker.network.bridge.trusted_host_interfaces={wireguard_interface} {bridge_name} >/dev/null\nfi\nactual_subnet=$(docker network inspect {bridge_name} --format '{{{{(index .IPAM.Config 0).Subnet}}}}')\nactual_gateway=$(docker network inspect {bridge_name} --format '{{{{(index .IPAM.Config 0).Gateway}}}}')\nactual_gateway_mode=$(docker network inspect {bridge_name} --format '{{{{index .Options \"com.docker.network.bridge.gateway_mode_ipv4\"}}}}')\nactual_trusted_interfaces=$(docker network inspect {bridge_name} --format '{{{{index .Options \"com.docker.network.bridge.trusted_host_interfaces\"}}}}')",
        subnet = params.container_subnet,
        gateway = params.bridge_gateway,
    )
}

fn render_podman_network(params: &BridgeScriptParams<'_>) -> String {
    let bridge_name = params.bridge_name;
    let bridge_interface = params.bridge_interface;
    format!(
        "if ! podman network inspect {bridge_name} >/dev/null 2>&1; then\n  podman network create --subnet {subnet} --gateway {gateway} --interface-name {bridge_interface} {bridge_name} >/dev/null\nfi\n\
         if ! ip link show {bridge_interface} >/dev/null 2>&1; then\n\
           ip link add name {bridge_interface} type bridge\n\
         fi\n\
         ip link set {bridge_interface} up\n\
         ip address replace {gateway}/{prefix} dev {bridge_interface}\n\
         actual_subnet=$(podman network inspect {bridge_name} --format '{{{{(index .Subnets 0).Subnet}}}}')\n\
         actual_gateway=$(podman network inspect {bridge_name} --format '{{{{(index .Subnets 0).Gateway}}}}')",
        subnet = params.container_subnet,
        gateway = params.bridge_gateway,
        prefix = params.container_subnet.prefix(),
    )
}

fn engine_option_validation(engine: BridgeEngineKind, params: &BridgeScriptParams<'_>) -> String {
    let bridge_name = params.bridge_name;
    let wireguard_interface = params.wireguard_interface;
    match engine {
        BridgeEngineKind::Docker => {
            format!(
                "test \"$actual_gateway_mode\" = \"routed\" || {{ echo \"{bridge_name} Docker network is not in routed gateway mode. Stop attached containers with: docker ps --filter network={bridge_name}. Remove the incompatible bridge with: docker network rm {bridge_name}. Then rerun jiji network setup from the deployment machine.\" >&2; exit 1; }}\ntest \"$actual_trusted_interfaces\" = \"{wireguard_interface}\" || {{ echo \"{bridge_name} Docker network does not trust {wireguard_interface}. Stop attached containers with: docker ps --filter network={bridge_name}. Remove the incompatible bridge with: docker network rm {bridge_name}. Then rerun jiji network setup from the deployment machine.\" >&2; exit 1; }}"
            )
        }
        BridgeEngineKind::Podman => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> (Ipv4Cidr, Ipv4Addr, Ipv4Cidr) {
        (
            "198.18.1.0/24".parse().unwrap(),
            "198.18.1.1".parse().unwrap(),
            "10.244.0.0/16".parse().unwrap(),
        )
    }

    #[test]
    fn docker_script_uses_routed_bridge_and_strict_drift_checks() {
        let (subnet, gateway, container_cidr) = params();
        let peers = vec!["203.0.113.20".parse().unwrap()];
        let script_params = BridgeScriptParams {
            bridge_name: "jiji-demo",
            bridge_interface: "jijibdemo",
            wireguard_interface: "jijidemo",
            container_subnet: subnet,
            bridge_gateway: gateway,
            dns_address: "198.18.1.2".parse().unwrap(),
            container_cidr,
            wireguard_port: 51820,
            peer_public_ips: &peers,
            public_host: "203.0.113.10",
        };
        let rendered = render_restore_script(BridgeEngineKind::Docker, &script_params);

        assert!(rendered.contains("enable_ip_masquerade=false"));
        assert!(rendered.contains("gateway_mode_ipv4=routed"));
        assert!(rendered.contains("trusted_host_interfaces=jijidemo"));
        assert!(rendered.contains("actual_subnet"));
        assert!(rendered.contains("actual_gateway"));
        assert!(rendered.contains("docker ps --filter network=jiji-demo"));
        assert!(rendered.contains("docker network rm jiji-demo"));
        assert!(rendered
            .contains("ensure_rule INPUT -p udp -s 203.0.113.20/32 --dport 51820 -j ACCEPT"));
    }

    #[test]
    fn podman_script_uses_planned_subnet_gateway_and_interface() {
        let (subnet, gateway, container_cidr) = params();
        let script_params = BridgeScriptParams {
            bridge_name: "jiji-demo",
            bridge_interface: "jijibdemo",
            wireguard_interface: "jijidemo",
            container_subnet: subnet,
            bridge_gateway: gateway,
            dns_address: "198.18.1.2".parse().unwrap(),
            container_cidr,
            wireguard_port: 51820,
            peer_public_ips: &[],
            public_host: "203.0.113.10",
        };
        let rendered = render_restore_script(BridgeEngineKind::Podman, &script_params);

        assert!(rendered.contains("podman network inspect jiji-demo"));
        assert!(rendered.contains("podman network create"));
        assert!(rendered.contains("--interface-name jijibdemo"));
        assert!(rendered.contains(&format!(
            "ip address replace {}/{} dev jijibdemo",
            gateway,
            subnet.prefix()
        )));
        assert!(!rendered.contains("gateway_mode_ipv4"));
    }

    #[test]
    fn existing_validation_never_removes_the_network() {
        let (subnet, gateway, container_cidr) = params();
        let script_params = BridgeScriptParams {
            bridge_name: "jiji-demo",
            bridge_interface: "jijibdemo",
            wireguard_interface: "jijidemo",
            container_subnet: subnet,
            bridge_gateway: gateway,
            dns_address: "198.18.1.2".parse().unwrap(),
            container_cidr,
            wireguard_port: 51820,
            peer_public_ips: &[],
            public_host: "203.0.113.10",
        };
        let rendered = render_existing_validation_command(BridgeEngineKind::Docker, &script_params);
        assert!(rendered.contains("docker network inspect jiji-demo"));
        assert!(rendered.contains("docker ps --filter network=jiji-demo"));
        assert!(!rendered.contains("docker network rm jiji-demo;"));
    }
}
