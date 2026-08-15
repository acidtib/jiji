use crate::ServerPlan;
use jiji_config::ContainerEngine;
use std::net::Ipv4Addr;

/// Which network namespace a container attaches to. Replaces a bare `Option<String>` "shared
/// with" field because there are now three genuinely different shapes to render, not two.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkTarget {
    /// The project bridge, with a leased `--ip` and `--dns*` flags (`network_mode: bridge`, the
    /// default).
    Bridge,
    /// A `network_mode: service:<other>` dependent: the upstream service's current container
    /// name, joined via `--network container:<name>`. `args()` omits `--ip`/`--dns`/
    /// `--dns-search`/`--dns-option` entirely, since a namespace-sharing container has no address
    /// or DNS configuration of its own -- it inherits the upstream's.
    SharedContainer(String),
    /// `network_mode: host`: shares the host's network namespace via `--network host`. Unlike
    /// `SharedContainer`, this still renders `--dns*` flags (pointed at the server's DNS agent),
    /// since a host-networked container inherits nothing else's resolver configuration.
    Host,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkedContainerRun {
    pub engine: ContainerEngine,
    pub container_name: String,
    pub image: String,
    pub address: Ipv4Addr,
    pub dns_address: Ipv4Addr,
    pub bridge_name: String,
    pub bridge_interface: String,
    pub network_target: NetworkTarget,
    pub extra_args: Vec<String>,
    pub command: Vec<String>,
}

impl NetworkedContainerRun {
    pub fn dynamic(
        engine: ContainerEngine,
        container_name: impl Into<String>,
        image: impl Into<String>,
        address: Ipv4Addr,
        server: &ServerPlan,
    ) -> Self {
        Self {
            engine,
            container_name: container_name.into(),
            image: image.into(),
            address,
            dns_address: server.dns_address,
            bridge_name: server.bridge_name.clone(),
            bridge_interface: server.bridge_interface.clone(),
            network_target: NetworkTarget::Bridge,
            extra_args: Vec::new(),
            command: Vec::new(),
        }
    }

    /// A `network_mode: service:<other>` dependent: shares `target_container`'s network
    /// namespace instead of getting its own dynamically-leased bridge address. `address` is the
    /// upstream's current address (kept for observability/health-check use, but never rendered
    /// into the run command -- see `NetworkTarget::SharedContainer` above).
    pub fn shared(
        engine: ContainerEngine,
        container_name: impl Into<String>,
        image: impl Into<String>,
        target_container: impl Into<String>,
        address: Ipv4Addr,
        server: &ServerPlan,
    ) -> Self {
        Self {
            engine,
            container_name: container_name.into(),
            image: image.into(),
            address,
            dns_address: server.dns_address,
            bridge_name: server.bridge_name.clone(),
            bridge_interface: server.bridge_interface.clone(),
            network_target: NetworkTarget::SharedContainer(target_container.into()),
            extra_args: Vec::new(),
            command: Vec::new(),
        }
    }

    /// A `network_mode: host` service: shares the host's own network namespace. `address` is the
    /// server's `management_address` (the WireGuard mesh address), kept for the `jiji.lease`
    /// label/catalog/health-check role the leased address plays for `dynamic()`, since a
    /// host-networked container has no address lease of its own.
    pub fn host(
        engine: ContainerEngine,
        container_name: impl Into<String>,
        image: impl Into<String>,
        address: Ipv4Addr,
        server: &ServerPlan,
    ) -> Self {
        Self {
            engine,
            container_name: container_name.into(),
            image: image.into(),
            address,
            dns_address: server.dns_address,
            bridge_name: server.bridge_name.clone(),
            bridge_interface: server.bridge_interface.clone(),
            network_target: NetworkTarget::Host,
            extra_args: Vec::new(),
            command: Vec::new(),
        }
    }

    pub fn args(&self) -> Vec<String> {
        let mut args = vec![
            self.engine.to_string(),
            "run".to_string(),
            "--name".to_string(),
            self.container_name.clone(),
        ];
        match &self.network_target {
            NetworkTarget::SharedContainer(target) => {
                args.push("--network".to_string());
                args.push(format!("container:{target}"));
            }
            NetworkTarget::Bridge => {
                args.push("--network".to_string());
                args.push(self.bridge_name.clone());
                args.push("--ip".to_string());
                args.push(self.address.to_string());
                args.extend(self.dns_args());
            }
            NetworkTarget::Host => {
                args.push("--network".to_string());
                args.push("host".to_string());
                args.extend(self.dns_args());
            }
        }
        args.extend(self.extra_args.clone());
        args.push(self.image.clone());
        args.extend(self.command.clone());
        args
    }

    /// `--dns`/`--dns-search`/`--dns-option` flags shared by `Bridge` and `Host`: both need
    /// `.jiji` resolution, unlike `SharedContainer`, which inherits its resolver config from
    /// whatever container it joins.
    fn dns_args(&self) -> Vec<String> {
        vec![
            "--dns".to_string(),
            self.dns_address.to_string(),
            "--dns-search".to_string(),
            jiji_core::DEFAULT_SERVICE_DOMAIN.to_string(),
            "--dns-option".to_string(),
            "ndots:1".to_string(),
        ]
    }

    pub fn shell_command(&self) -> String {
        self.args()
            .iter()
            .map(|argument| shell_escape(argument))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn shell_escape(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "_-./:@".contains(character))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NetworkPlanner;

    #[test]
    fn dynamic_run_pins_address_dns_and_project_bridge() {
        let config: jiji_config::Config = serde_yaml::from_str(
            "project: demo\nbuilder: { engine: docker }\nservers:\n  app: { host: 203.0.113.10 }\nservices:\n  web: { image: nginx, servers: [app] }\n",
        )
        .unwrap();
        let plan = NetworkPlanner::new().plan(&config).unwrap();
        let server = &plan.servers["app"];
        let run = NetworkedContainerRun::dynamic(
            ContainerEngine::Docker,
            "demo-web-abc",
            "nginx:latest",
            "198.18.1.20".parse().unwrap(),
            server,
        );
        let command = run.shell_command();
        assert!(command.contains("--name demo-web-abc"));
        assert!(command.contains("--ip 198.18.1.20"));
        assert!(command.contains(&format!("--dns {}", server.dns_address)));
        assert!(command.contains(&format!("--network {}", server.bridge_name)));
    }

    #[test]
    fn shared_run_joins_the_target_container_and_omits_its_own_addressing() {
        let config: jiji_config::Config = serde_yaml::from_str(
            "project: demo\nbuilder: { engine: docker }\nservers:\n  app: { host: 203.0.113.10 }\nservices:\n  web: { image: nginx, servers: [app] }\n",
        )
        .unwrap();
        let plan = NetworkPlanner::new().plan(&config).unwrap();
        let server = &plan.servers["app"];
        let run = NetworkedContainerRun::shared(
            ContainerEngine::Docker,
            "demo-qbittorrent-abc",
            "qbittorrent:latest",
            "demo-gluetun-def",
            "198.18.1.20".parse().unwrap(),
            server,
        );
        let command = run.shell_command();
        assert!(command.contains("--name demo-qbittorrent-abc"));
        assert!(command.contains("--network container:demo-gluetun-def"));
        assert!(!command.contains("--ip"));
        assert!(!command.contains("--dns"));
        assert!(!command.contains(&server.bridge_name));
    }

    #[test]
    fn host_run_uses_network_host_and_keeps_dns() {
        let config: jiji_config::Config = serde_yaml::from_str(
            "project: demo\nbuilder: { engine: docker }\nservers:\n  app: { host: 203.0.113.10 }\nservices:\n  web: { image: nginx, servers: [app] }\n",
        )
        .unwrap();
        let plan = NetworkPlanner::new().plan(&config).unwrap();
        let server = &plan.servers["app"];
        let run = NetworkedContainerRun::host(
            ContainerEngine::Docker,
            "demo-web-abc",
            "nginx:latest",
            server.management_address,
            server,
        );
        let command = run.shell_command();
        assert!(command.contains("--name demo-web-abc"));
        assert!(command.contains("--network host"));
        assert!(command.contains(&format!("--dns {}", server.dns_address)));
        assert!(!command.contains("--ip"));
        assert!(!command.contains(&server.bridge_name));
    }
}
