use std::path::Path;

use jiji_config::{load_config, validate_config};
use jiji_network::NetworkPlanner;
use jiji_tui::Ui;

pub fn run(environment: Option<&str>, config_file: Option<&str>) -> anyhow::Result<()> {
    let start = std::env::current_dir()?;
    let config_path = config_file.map(Path::new);
    let (config, path) = load_config(environment, config_path, &start)?;
    let validation = validate_config(&config);
    if !validation.valid {
        for error in &validation.errors {
            Ui::error(&format!("{}: {}", error.path, error.message));
        }
        anyhow::bail!(
            "Configuration loaded from {} is invalid; fix the errors above and retry",
            path.display()
        );
    }
    let plan = NetworkPlanner::new().plan(&config)?;

    println!("generation {}", plan.generation);
    println!("management {}", plan.management_cidr);
    println!("containers {}", plan.container_cidr);
    println!(
        "project {} wireguard-interface={} bridge-interface={} bridge-network={} wireguard-port={}",
        plan.project,
        jiji_network::wireguard_interface_name(&plan.project),
        jiji_network::bridge_interface_name(&plan.project),
        jiji_network::bridge_network_name(&plan.project),
        jiji_network::wireguard_port(&plan.project),
    );
    for server in plan.servers.values() {
        println!(
            "server {} host={} management={} subnet={} gateway={} dns={} proxy={}",
            server.name,
            server.public_host,
            server.management_address,
            server.container_subnet,
            server.bridge_gateway,
            server.dns_address,
            server.proxy_address
        );
    }
    for endpoint in plan.endpoints.values() {
        println!(
            "endpoint {} dns={} server-dns={} vip={} backend-a={} backend-b={}",
            endpoint.identity,
            endpoint.dns_name,
            endpoint.server_dns_name,
            endpoint.address,
            endpoint.backend_addresses[0],
            endpoint.backend_addresses[1]
        );
    }
    Ok(())
}
