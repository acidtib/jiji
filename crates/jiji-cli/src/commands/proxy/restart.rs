use jiji_config::validate_config;
use jiji_network::NetworkPlanner;
use jiji_ssh::{SshPool, SshSession};
use jiji_tui::Ui;

use crate::{proxy, ssh_adapter};

pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    hosts: Option<&str>,
    services: Option<&str>,
) -> anyhow::Result<()> {
    Ui::section("Proxy Restart:");
    if services.is_some() {
        anyhow::bail!(
            "`jiji proxy restart` does not accept -S/--services: jiji-proxy is shared by every service on a host. Use -H/--hosts to select servers instead."
        );
    }

    let start = std::env::current_dir()?;
    let (config, path) = crate::config_loading::load_config_for_ssh(
        environment,
        config_file.map(std::path::Path::new),
        &start,
    )
    .await?;
    let validation = validate_config(&config);
    if !validation.valid {
        for error in validation.errors {
            Ui::error(&format!("{}: {}", error.path, error.message));
        }
        anyhow::bail!("Configuration is invalid; fix the errors above and try again");
    }
    let ssh = config.ssh.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "No `ssh:` section configured in {}. Add at least `ssh.user:` before running proxy restart.",
            path.display()
        )
    })?;
    let plan = NetworkPlanner::new()
        .plan(&config)
        .map_err(|error| anyhow::anyhow!("Could not build the proxy network plan: {error}"))?;
    let filters = split_comma_trimmed(hosts);
    let selected = plan.select_hosts(&filters)?;
    if selected.is_empty() {
        anyhow::bail!("No servers are configured. Add a `servers:` entry and retry.");
    }

    Ui::warn(
        "Restarting jiji-proxy briefly interrupts every proxy route on each selected host. \
         jiji-proxy is shared across every jiji project on a host: recreating it also drops \
         any other project's network attachment, which is restored the next time that project \
         runs `jiji deploy`/`jiji server setup`/`jiji proxy restart`, but its routes are \
         unreachable until then.",
    );
    let mut operations = Vec::with_capacity(selected.len());
    for server_plan in selected {
        let name = server_plan.name.clone();
        let named_server = config.servers.get(&name).cloned().ok_or_else(|| {
            anyhow::anyhow!("Server '{name}' selected by the network plan is not configured")
        })?;
        let options = ssh_adapter::connect_options(&name, &named_server, &ssh)?;
        let engine = config.builder.engine;
        let network = if plan.enabled {
            Some(proxy::ProxyNetwork {
                bridge_name: server_plan.bridge_name.clone(),
                bridge_interface: server_plan.bridge_interface.clone(),
                proxy_address: server_plan.proxy_address,
                dns_address: server_plan.dns_address,
                public_host: proxy::parse_public_host(server_plan)?,
            })
        } else {
            None
        };
        operations.push(move || async move {
            let result = async {
                let session = SshSession::connect(&options).await?;
                let outcome = proxy::ensure_proxy(&session, engine, network, true).await;
                session.close().await;
                outcome
            }
            .await;
            (name, result)
        });
    }

    let pool = SshPool::new(ssh.max_concurrent_starts as usize);
    let outcomes = pool.execute_concurrent(operations).await;
    let mut failures = Vec::new();
    for (name, outcome) in outcomes {
        match outcome {
            Ok(_) => Ui::say(&format!("{name}: jiji-proxy restarted"), 1),
            Err(error) => {
                Ui::error(&format!("{name}: {error}"));
                failures.push((name, error.to_string()));
            }
        }
    }
    if !failures.is_empty() {
        anyhow::bail!(
            "Jiji-proxy restart failed for {} server(s). Fix the reported hosts and retry `jiji proxy restart`.",
            failures.len()
        );
    }
    Ok(())
}

fn split_comma_trimmed(value: Option<&str>) -> Vec<String> {
    value
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}
