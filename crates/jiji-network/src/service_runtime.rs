use crate::{NetworkPlan, ServerPlan, ServiceEndpointPlan};
use jiji_config::ContainerEngine;
use std::collections::BTreeMap;
use std::fmt;
use std::net::Ipv4Addr;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendSlot {
    A,
    B,
}

impl BackendSlot {
    pub fn other(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
        }
    }

    fn index(self) -> usize {
        match self {
            Self::A => 0,
            Self::B => 1,
        }
    }
}

impl fmt::Display for BackendSlot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::A => write!(formatter, "a"),
            Self::B => write!(formatter, "b"),
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ServiceRuntimeError {
    #[error(
        "Active slot state contains malformed line {line}: '{value}'. Repair or remove the state file and retry."
    )]
    MalformedState { line: usize, value: String },

    #[error(
        "Active slot state references unknown endpoint '{identity}'. Run network setup to reconcile the installed topology."
    )]
    UnknownEndpoint { identity: String },

    #[error("{0}")]
    Remote(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActiveSlotState {
    slots: BTreeMap<String, BackendSlot>,
}

impl ActiveSlotState {
    pub fn parse(input: &str) -> Result<Self, ServiceRuntimeError> {
        let mut slots = BTreeMap::new();
        for (index, line) in input.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Some((identity, slot)) = line.split_once('=') else {
                return Err(ServiceRuntimeError::MalformedState {
                    line: index + 1,
                    value: line.to_string(),
                });
            };
            let slot = match slot {
                "a" => BackendSlot::A,
                "b" => BackendSlot::B,
                _ => {
                    return Err(ServiceRuntimeError::MalformedState {
                        line: index + 1,
                        value: line.to_string(),
                    });
                }
            };
            if identity.is_empty() || slots.insert(identity.to_string(), slot).is_some() {
                return Err(ServiceRuntimeError::MalformedState {
                    line: index + 1,
                    value: line.to_string(),
                });
            }
        }
        Ok(Self { slots })
    }

    pub fn active_slot(&self, endpoint_identity: &str) -> Option<BackendSlot> {
        self.slots.get(endpoint_identity).copied()
    }

    pub fn deployment_slot(&self, endpoint_identity: &str) -> BackendSlot {
        self.active_slot(endpoint_identity)
            .map(BackendSlot::other)
            .unwrap_or(BackendSlot::A)
    }

    pub fn activate(&mut self, endpoint_identity: impl Into<String>, slot: BackendSlot) {
        self.slots.insert(endpoint_identity.into(), slot);
    }

    pub fn deactivate(&mut self, endpoint_identity: &str) {
        self.slots.remove(endpoint_identity);
    }

    pub fn retain(&mut self, mut keep: impl FnMut(&str) -> bool) {
        self.slots.retain(|identity, _| keep(identity));
    }

    pub fn render(&self) -> String {
        let mut output = String::new();
        for (identity, slot) in &self.slots {
            output.push_str(&format!("{identity}={slot}\n"));
        }
        output
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, BackendSlot)> {
        self.slots
            .iter()
            .map(|(identity, slot)| (identity.as_str(), *slot))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkedContainerRun {
    pub engine: ContainerEngine,
    pub container_name: String,
    pub image: String,
    pub address: Ipv4Addr,
    pub dns_address: Ipv4Addr,
    pub extra_args: Vec<String>,
    pub command: Vec<String>,
}

impl NetworkedContainerRun {
    pub fn for_endpoint(
        engine: ContainerEngine,
        container_name: impl Into<String>,
        image: impl Into<String>,
        endpoint: &ServiceEndpointPlan,
        server: &ServerPlan,
        slot: BackendSlot,
    ) -> Self {
        Self {
            engine,
            container_name: container_name.into(),
            image: image.into(),
            address: endpoint.backend_addresses[slot.index()],
            dns_address: server.dns_address,
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
            "--network".to_string(),
            "jiji".to_string(),
            "--ip".to_string(),
            self.address.to_string(),
            "--dns".to_string(),
            self.dns_address.to_string(),
            "--dns-search".to_string(),
            jiji_core::DEFAULT_SERVICE_DOMAIN.to_string(),
            "--dns-option".to_string(),
            "ndots:1".to_string(),
        ];
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceNatArtifacts {
    pub state: String,
    pub nftables: String,
}

impl ServiceNatArtifacts {
    pub fn render(
        plan: &NetworkPlan,
        active: &ActiveSlotState,
    ) -> Result<Self, ServiceRuntimeError> {
        let mut elements = Vec::new();
        for (identity, slot) in active.iter() {
            let endpoint = plan.endpoints.get(identity).ok_or_else(|| {
                ServiceRuntimeError::UnknownEndpoint {
                    identity: identity.to_string(),
                }
            })?;
            elements.push(format!(
                "{} : {}",
                endpoint.address,
                endpoint.backend_addresses[slot.index()]
            ));
        }
        let elements = if elements.is_empty() {
            String::new()
        } else {
            format!("\t\telements = {{ {} }}\n", elements.join(", "))
        };
        let nftables = format!(
            "delete table ip jiji_service_nat\n\
             table ip jiji_service_nat {{\n\
             \tmap backends {{\n\
             \t\ttype ipv4_addr : ipv4_addr\n\
             {elements}\
             \t}}\n\
             \tchain prerouting {{\n\
             \t\ttype nat hook prerouting priority dstnat - 5; policy accept;\n\
             \t\tdnat ip to ip daddr map @backends\n\
             \t}}\n\
             \tchain output {{\n\
             \t\ttype nat hook output priority dstnat - 5; policy accept;\n\
             \t\tdnat ip to ip daddr map @backends\n\
             \t}}\n\
             }}\n"
        );
        Ok(Self {
            state: active.render(),
            nftables,
        })
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
    use jiji_config::Config;

    fn plan() -> NetworkPlan {
        let config: Config = serde_yaml::from_str(
            r#"
project: demo
builder: { engine: docker }
servers:
  app: { host: 203.0.113.10 }
services:
  web:
    image: example/web
    hosts: [app]
"#,
        )
        .unwrap();
        NetworkPlanner::new().plan(&config).unwrap()
    }

    #[test]
    fn deployment_uses_inactive_slot_and_rollback_returns_to_previous_slot() {
        let plan = plan();
        let endpoint = &plan.endpoints["demo:web:app"];
        let mut state = ActiveSlotState::default();

        let first = state.deployment_slot(&endpoint.identity);
        assert_eq!(first, BackendSlot::A);
        state.activate(&endpoint.identity, first);
        let replacement = state.deployment_slot(&endpoint.identity);
        assert_eq!(replacement, BackendSlot::B);
        state.activate(&endpoint.identity, replacement);
        assert_eq!(state.deployment_slot(&endpoint.identity), BackendSlot::A);

        state.activate(&endpoint.identity, first);
        assert_eq!(state.active_slot(&endpoint.identity), Some(BackendSlot::A));
        state.deactivate(&endpoint.identity);
        assert_eq!(state.active_slot(&endpoint.identity), None);
    }

    #[test]
    fn replacement_and_stop_first_never_reuse_the_active_container_address() {
        let plan = plan();
        let endpoint = &plan.endpoints["demo:web:app"];
        let mut state = ActiveSlotState::default();
        state.activate(&endpoint.identity, BackendSlot::A);

        let replacement_slot = state.deployment_slot(&endpoint.identity);
        let old = NetworkedContainerRun::for_endpoint(
            ContainerEngine::Docker,
            "demo-web-old",
            "example/web:v1",
            endpoint,
            &plan.servers["app"],
            BackendSlot::A,
        );
        let replacement = NetworkedContainerRun::for_endpoint(
            ContainerEngine::Docker,
            "demo-web",
            "example/web:v2",
            endpoint,
            &plan.servers["app"],
            replacement_slot,
        );

        assert_ne!(old.address, replacement.address);
        assert_eq!(replacement_slot, BackendSlot::B);
        assert_eq!(state.deployment_slot(&endpoint.identity), replacement_slot);
    }

    #[test]
    fn run_command_always_contains_planned_network_ip_and_dns() {
        let plan = plan();
        let endpoint = &plan.endpoints["demo:web:app"];
        let server = &plan.servers["app"];
        for engine in [ContainerEngine::Docker, ContainerEngine::Podman] {
            for slot in [BackendSlot::A, BackendSlot::B] {
                let command = NetworkedContainerRun::for_endpoint(
                    engine,
                    "demo-web",
                    "example/web:v1",
                    endpoint,
                    server,
                    slot,
                )
                .shell_command();
                assert!(command.contains("--network jiji"));
                assert!(command.contains(&format!(
                    "--ip {}",
                    endpoint.backend_addresses[slot.index()]
                )));
                assert!(command.contains(&format!("--dns {}", server.dns_address)));
                assert!(command.contains("--dns-search jiji --dns-option ndots:1"));
            }
        }
    }

    #[test]
    fn restart_and_reboot_reuse_the_active_backend_address() {
        let plan = plan();
        let endpoint = &plan.endpoints["demo:web:app"];
        let mut state = ActiveSlotState::default();
        state.activate(&endpoint.identity, BackendSlot::B);
        let persisted = state.render();
        let restored = ActiveSlotState::parse(&persisted).unwrap();
        let active = restored.active_slot(&endpoint.identity).unwrap();

        let run = NetworkedContainerRun::for_endpoint(
            ContainerEngine::Docker,
            "demo-web",
            "example/web:v1",
            endpoint,
            &plan.servers["app"],
            active,
        );
        assert_eq!(run.address, endpoint.backend_addresses[1]);
    }

    #[test]
    fn nat_artifacts_map_stable_vip_to_active_backend() {
        let plan = plan();
        let endpoint = &plan.endpoints["demo:web:app"];
        let mut state = ActiveSlotState::default();
        state.activate(&endpoint.identity, BackendSlot::B);

        let artifacts = ServiceNatArtifacts::render(&plan, &state).unwrap();
        assert!(artifacts.nftables.contains(&format!(
            "{} : {}",
            endpoint.address, endpoint.backend_addresses[1]
        )));
        assert!(artifacts
            .nftables
            .contains("dnat ip to ip daddr map @backends"));
        assert!(artifacts
            .nftables
            .starts_with("delete table ip jiji_service_nat\n"));
        assert!(!artifacts.nftables.contains("flush table"));
        assert_eq!(artifacts.state, "demo:web:app=b\n");
    }

    #[test]
    fn malformed_or_stale_state_is_rejected() {
        assert!(matches!(
            ActiveSlotState::parse("demo:web:app=c\n"),
            Err(ServiceRuntimeError::MalformedState { .. })
        ));
        let plan = plan();
        let state = ActiveSlotState::parse("missing:endpoint:host=a\n").unwrap();
        assert!(matches!(
            ServiceNatArtifacts::render(&plan, &state),
            Err(ServiceRuntimeError::UnknownEndpoint { .. })
        ));
    }
}
