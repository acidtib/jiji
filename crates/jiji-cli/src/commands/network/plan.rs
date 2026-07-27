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

    Ui::section("Network Plan:");
    Ui::say(&format!("Generation: {}", plan.generation), 1);
    Ui::say(&format!("Management CIDR: {}", plan.management_cidr), 1);
    Ui::say(&format!("Container CIDR: {}", plan.container_cidr), 1);
    Ui::say(&format!("Project: {}", plan.project), 1);
    Ui::say(
        &format!(
            "WireGuard: interface={} port={}",
            jiji_network::wireguard_interface_name(&plan.project),
            jiji_network::wireguard_port(&plan.project),
        ),
        2,
    );
    Ui::say(
        &format!(
            "Bridge: interface={} network={}",
            jiji_network::bridge_interface_name(&plan.project),
            jiji_network::bridge_network_name(&plan.project),
        ),
        2,
    );

    Ui::section("Servers:");
    for server in plan.servers.values() {
        Ui::say(&format!("{} ({})", server.name, server.public_host), 1);
        Ui::say(&format!("management: {}", server.management_address), 2);
        Ui::say(&format!("subnet:     {}", server.container_subnet), 2);
        Ui::say(&format!("gateway:    {}", server.bridge_gateway), 2);
        Ui::say(&format!("dns:        {}", server.dns_address), 2);
        Ui::say(&format!("proxy:      {}", server.proxy_address), 2);
    }

    Ui::section("Endpoints:");
    for endpoint in plan.endpoints.values() {
        Ui::say(&endpoint.identity, 1);
        Ui::say(&format!("dns:        {}", endpoint.dns_name), 2);
        Ui::say(&format!("server-dns: {}", endpoint.server_dns_name), 2);
        Ui::say(&format!("vip:        {}", endpoint.address), 2);
        Ui::say(
            &format!(
                "backends:   a={} b={}",
                endpoint.backend_addresses[0], endpoint.backend_addresses[1]
            ),
            2,
        );
    }

    Ok(())
}
