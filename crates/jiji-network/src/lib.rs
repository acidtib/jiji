mod bridge_script;
mod cidr;
mod error;
mod naming;
mod planner;
mod proxy_script;
mod service_runtime;

pub use bridge_script::{
    render_existing_validation_command, render_restore_script, BridgeEngineKind, BridgeScriptParams,
};
pub use cidr::Ipv4Cidr;
pub use error::NetworkPlanError;
pub use naming::{
    bridge_interface_name, bridge_network_name, catalog_replication_port,
    membership_replication_port, service_nat_table_name, systemd_unit_slug,
    wireguard_interface_name, wireguard_port,
};
pub use planner::{
    FirewallPlan, NetworkPlan, NetworkPlanner, RoutePlan, ServerPlan, ServiceEndpointPlan,
    WireGuardPeerPlan, CONTAINER_SERVER_PREFIX,
};
pub use proxy_script::{
    attached_address, config_fingerprint, is_missing_container_error, render_nftables,
    render_run_command, surviving_proxy_address, ProxyRunNetwork, CERTS_DIR, CONFIG_VOLUME,
    CONTAINER_NAME, IMAGE, INGRESS_TABLE, INTERNAL_HTTPS_PORT, INTERNAL_HTTP_PORT,
};
pub use service_runtime::NetworkedContainerRun;
