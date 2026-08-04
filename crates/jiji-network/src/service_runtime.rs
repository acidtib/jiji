use crate::ServerPlan;
use jiji_config::ContainerEngine;
use std::net::Ipv4Addr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkedContainerRun {
    pub engine: ContainerEngine,
    pub container_name: String,
    pub image: String,
    pub address: Ipv4Addr,
    pub dns_address: Ipv4Addr,
    pub bridge_name: String,
    pub bridge_interface: String,
    /// Set only for a `network_mode: service:<other>` dependent: the upstream service's current
    /// container name to join via `--network container:<name>`. When set, `args()` omits
    /// `--ip`/`--dns`/`--dns-search`/`--dns-option` entirely, since a namespace-sharing container
    /// has no address or DNS configuration of its own -- it inherits the upstream's.
    pub shared_with_container: Option<String>,
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
            shared_with_container: None,
            extra_args: Vec::new(),
            command: Vec::new(),
        }
    }

    /// A `network_mode: service:<other>` dependent: shares `target_container`'s network
    /// namespace instead of getting its own dynamically-leased bridge address. `address` is the
    /// upstream's current address (kept for observability/health-check use, but never rendered
    /// into the run command -- see `shared_with_container` above).
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
            shared_with_container: Some(target_container.into()),
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
        match &self.shared_with_container {
            Some(target) => {
                args.push("--network".to_string());
                args.push(format!("container:{target}"));
            }
            None => {
                args.push("--network".to_string());
                args.push(self.bridge_name.clone());
                args.push("--ip".to_string());
                args.push(self.address.to_string());
                args.push("--dns".to_string());
                args.push(self.dns_address.to_string());
                args.push("--dns-search".to_string());
                args.push(jiji_core::DEFAULT_SERVICE_DOMAIN.to_string());
                args.push("--dns-option".to_string());
                args.push("ndots:1".to_string());
            }
        }
        args.extend(self.extra_args.clone());
        args.push(self.image.clone());
        args.extend(self.command.clone());
        args
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
}
