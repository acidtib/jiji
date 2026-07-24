mod cidr;
mod error;
mod naming;
mod planner;
mod service_runtime;

pub use cidr::Ipv4Cidr;
pub use error::NetworkPlanError;
pub use naming::{
    bridge_interface_name, bridge_network_name, service_nat_table_name, systemd_unit_slug,
    wireguard_interface_name, wireguard_port,
};
pub use planner::{
    DnsRecord, FirewallPlan, NetworkPlan, NetworkPlanner, RoutePlan, ServerPlan,
    ServiceEndpointPlan, WireGuardPeerPlan, CONTAINER_SERVER_PREFIX,
};
pub use service_runtime::{
    ActiveSlotState, BackendSlot, NetworkedContainerRun, ServiceNatArtifacts, ServiceRuntimeError,
};
