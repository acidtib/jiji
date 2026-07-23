use jiji_config::{load_config, validate_config, Config, NamedServer, Ssh};
use jiji_network::NetworkPlanner;
use jiji_ssh::{SshPool, SshSession};
use jiji_tui::Ui;

use crate::commands::network;
use crate::engine::{self, EngineStatus};
use crate::proxy::{self, ProxyStatus};
use crate::ssh_adapter;

pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    hosts: Option<&str>,
) -> anyhow::Result<()> {
    Ui::section("Server Setup:");

    let start = std::env::current_dir()?;
    let config_path = config_file.map(std::path::Path::new);
    let (config, path) = load_config(environment, config_path, &start)?;
    Ui::say(&format!("Configuration loaded from: {}", path.display()), 1);

    let validation = validate_config(&config);
    if !validation.valid {
        Ui::error(&format!(
            "Configuration validation failed with {} error(s):",
            validation.errors.len()
        ));
        for e in &validation.errors {
            Ui::say(&format!("{}: {}", e.path, e.message), 1);
        }
        anyhow::bail!("Configuration is invalid; fix the errors above and try again");
    }

    let ssh = config.ssh.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "No `ssh:` section configured in {}. Add at least `ssh.user:` before running server setup.",
            path.display()
        )
    })?;

    let mut servers: Vec<(String, NamedServer)> = config.servers.clone().into_iter().collect();
    servers.sort_by(|a, b| a.0.cmp(&b.0));

    if servers.is_empty() {
        anyhow::bail!(
            "No servers defined in {}. Add a `servers:` entry before running server setup.",
            path.display()
        );
    }

    if let Some(pattern_list) = hosts {
        servers = filter_by_hosts(servers, pattern_list)?;
    }

    Ui::say(
        &format!(
            "Targeting {} server(s): {}",
            servers.len(),
            servers
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        1,
    );

    let pool = SshPool::new(ssh.max_concurrent_starts as usize);
    let mut connect_operations = Vec::with_capacity(servers.len());
    for (name, server) in &servers {
        connect_operations.push(ssh_adapter::connect_options(name, server, &ssh)?);
    }

    Ui::section("Connecting:");
    let operations: Vec<_> = connect_operations
        .into_iter()
        .map(|options| move || async move { SshSession::connect(&options).await })
        .collect();
    let connections = pool.execute_concurrent(operations).await;

    let mut failures: Vec<(String, String)> = Vec::new();
    let mut sessions: Vec<(String, SshSession)> = Vec::new();
    for ((name, server), connection) in servers.iter().zip(connections) {
        match connection {
            Ok(session) => {
                Ui::say(&format!("{name} ({}): connected", server.host), 1);
                sessions.push((name.clone(), session));
            }
            Err(err) => {
                Ui::error(&format!("{name} ({}): {err}", server.host));
                failures.push((name.clone(), err.to_string()));
            }
        }
    }

    if sessions.is_empty() {
        anyhow::bail!("Could not connect to any server; see the errors above");
    }

    Ui::section("Installing Container Engine:");
    let engine = config.builder.engine;
    for (name, session) in &sessions {
        Ui::say(&format!("{name} ({}):", session.host()), 1);
        match engine::ensure_engine(session, engine).await {
            Ok(EngineStatus::AlreadyInstalled(version)) => {
                Ui::say(&format!("{engine} already installed ({version})"), 2);
            }
            Ok(EngineStatus::Installed(version)) => {
                Ui::say(&format!("{engine} installed ({version})"), 2);
            }
            Err(err) => {
                Ui::error(&format!("  {err}"));
                failures.push((name.clone(), err.to_string()));
            }
        }
        session.close().await;
    }

    if !failures.is_empty() {
        Ui::error(&format!("\n{} server(s) failed:", failures.len()));
        for (name, message) in &failures {
            Ui::say(&format!("{name}: {message}"), 1);
        }
        anyhow::bail!("Server setup failed for {} server(s)", failures.len());
    }

    network::setup::run(environment, config_file, hosts)
        .await
        .map_err(|error| {
            anyhow::anyhow!(
                "Container engine setup succeeded, but complete network setup failed: {error}"
            )
        })?;

    setup_proxies(&config, &servers, &ssh).await?;

    Ui::success("\nAll servers are ready.");
    Ok(())
}

async fn setup_proxies(
    config: &Config,
    servers: &[(String, NamedServer)],
    ssh: &Ssh,
) -> anyhow::Result<()> {
    let plan = NetworkPlanner::new()
        .plan(config)
        .map_err(|error| anyhow::anyhow!("Could not build the proxy network plan: {error}"))?;
    let dns_enabled = plan.enabled;

    Ui::section("Configuring Kamal Proxy:");
    let pool = SshPool::new(ssh.max_concurrent_starts as usize);
    let mut connect_operations = Vec::with_capacity(servers.len());
    for (name, server) in servers {
        connect_operations.push(ssh_adapter::connect_options(name, server, ssh)?);
    }
    let operations: Vec<_> = connect_operations
        .into_iter()
        .map(|options| move || async move { SshSession::connect(&options).await })
        .collect();
    let connections = pool.execute_concurrent(operations).await;

    let mut failures = Vec::new();
    for ((name, _), connection) in servers.iter().zip(connections) {
        let session = match connection {
            Ok(session) => session,
            Err(error) => {
                failures.push((name.clone(), error.to_string()));
                continue;
            }
        };
        let server_plan = &plan.servers[name];
        let network = dns_enabled.then_some(proxy::ProxyNetwork {
            dns_address: server_plan.dns_address,
            proxy_address: server_plan.proxy_address,
        });
        match proxy::ensure_proxy(&session, config.builder.engine, network).await {
            Ok(ProxyStatus::AlreadyRunning) => {
                Ui::say(
                    &format!("{name}: kamal-proxy already configured and running"),
                    1,
                );
            }
            Ok(ProxyStatus::Started) => {
                Ui::say(&format!("{name}: kamal-proxy configured and running"), 1);
            }
            Err(error) => {
                Ui::error(&format!("{name}: {error}"));
                failures.push((name.clone(), error.to_string()));
            }
        }
        session.close().await;
    }

    if !failures.is_empty() {
        for (name, error) in &failures {
            Ui::say(&format!("{name}: {error}"), 1);
        }
        anyhow::bail!(
            "Kamal proxy setup failed for {} server(s). Fix the reported hosts and retry `jiji server setup`.",
            failures.len()
        );
    }

    Ok(())
}

/// Filters `servers` down to those whose `host` value matches at least one comma-separated
/// `*`-wildcard pattern. Warns on patterns that match nothing, and fails only if the filter
/// empties the whole set.
fn filter_by_hosts(
    servers: Vec<(String, NamedServer)>,
    pattern_list: &str,
) -> anyhow::Result<Vec<(String, NamedServer)>> {
    let patterns: Vec<&str> = pattern_list.split(',').map(str::trim).collect();
    let mut matched_any = vec![false; patterns.len()];

    let filtered: Vec<(String, NamedServer)> = servers
        .into_iter()
        .filter(|(_, server)| {
            patterns.iter().enumerate().any(|(i, pattern)| {
                let is_match = jiji_core::matches_pattern(&server.host, pattern);
                if is_match {
                    matched_any[i] = true;
                }
                is_match
            })
        })
        .collect();

    for (pattern, matched) in patterns.iter().zip(matched_any.iter()) {
        if !matched {
            Ui::warn(&format!("No server host matched pattern '{pattern}'"));
        }
    }

    if filtered.is_empty() {
        anyhow::bail!("No servers matched --hosts filter '{pattern_list}'");
    }

    Ok(filtered)
}
