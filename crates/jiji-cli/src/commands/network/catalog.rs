//! `jiji network catalog`: read-only inspection of each selected server's locally replicated
//! service catalog. Mirrors `commands/network/membership.rs::publish`'s `membership-export` shape
//! (an SSH-exec'd agent subcommand, not a direct socket call) since the CLI has no WireGuard-mesh
//! reachability to the agent's Unix socket -- it can only ever reach the agent by SSHing to the
//! host that runs it.

use jiji_agent::catalog::CatalogRecord;
use jiji_agent::AgentPaths;
use jiji_config::validate_config;
use jiji_network::NetworkPlanner;
use jiji_ssh::SshSession;
use jiji_tui::Ui;

use crate::ssh_adapter;

pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    hosts: Option<&str>,
) -> anyhow::Result<()> {
    let start = std::env::current_dir()?;
    let (config, _path) = crate::config_loading::load_config_for_ssh(
        environment,
        config_file.map(std::path::Path::new),
        &start,
    )
    .await?;
    let validation = validate_config(&config);
    if !validation.valid {
        anyhow::bail!("Configuration is invalid; fix it before inspecting the catalog");
    }
    let ssh = config
        .ssh
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No `ssh:` section is configured"))?;
    let plan = NetworkPlanner::new()
        .plan(&config)
        .map_err(|error| anyhow::anyhow!("Could not build the network plan: {error}"))?;
    let filters = split_comma_trimmed(hosts);
    let selected = plan.select_hosts(&filters)?;
    if selected.is_empty() {
        anyhow::bail!("No servers are configured. Add a `servers:` entry and retry.");
    }

    Ui::section("Service Catalog:");
    let started = std::time::Instant::now();
    let hosts: Vec<String> = selected.iter().map(|s| s.name.clone()).collect();
    let progress =
        jiji_tui::ServerSetupProgress::with_title(hosts.clone(), "Fetching catalog".to_string());
    let handle = progress.handle();
    let paths = AgentPaths::default_for_project(&config.project);
    let mut failures = Vec::new();
    for server_plan in &selected {
        let name = server_plan.name.clone();
        handle.set_status(&name, "fetching");
        let named_server = config.servers.get(&name).cloned().ok_or_else(|| {
            anyhow::anyhow!("Server '{name}' selected by the network plan is not configured")
        })?;
        let options = ssh_adapter::connect_options(&name, &named_server, ssh)?;
        let result = fetch_catalog(&options, &paths).await;
        match result {
            Ok(records) => {
                handle.mark_success(&name, &format!("{} record(s)", records.len()));
                print_records(&name, &records)
            }
            Err(error) => {
                handle.mark_failed(&name, &error.to_string());
                Ui::say(&format!("{name}: {error}"), 1);
                failures.push((name, error.to_string()));
            }
        }
    }
    progress.finish();
    Ui::say(
        &format!(
            "Fetched from {} host(s) in {}",
            hosts.len(),
            jiji_tui::format_duration(started.elapsed())
        ),
        1,
    );
    if !failures.is_empty() {
        anyhow::bail!(
            "Could not read the catalog from {} server(s). Fix the reported hosts and retry.",
            failures.len()
        );
    }
    Ok(())
}

async fn fetch_catalog(
    options: &jiji_ssh::ConnectOptions,
    paths: &AgentPaths,
) -> anyhow::Result<Vec<CatalogRecord>> {
    let session = SshSession::connect(options).await?;
    let result = session
        .execute(&format!(
            "{} catalog-export --state-dir {}",
            paths.binary_path.display(),
            paths.state_dir.display()
        ))
        .await;
    session.close().await;
    let result = result?;
    if !result.success {
        anyhow::bail!(
            "jiji agent not reachable or not installed ({})",
            result.stderr.trim()
        );
    }
    Ok(serde_json::from_str(&result.stdout)?)
}

fn print_records(server: &str, records: &[CatalogRecord]) {
    if records.is_empty() {
        Ui::say(&format!("{server}: no catalog records"), 1);
        return;
    }
    for record in records {
        Ui::say(
            &format!(
                "{server}: {service} [{state:?}/{health:?}] owner={owner} address={address} \
                 deployment={deployment} image={image} rev={revision}",
                service = record.service,
                state = record.state,
                health = record.health,
                owner = record.owner_node_id,
                address = record.address,
                deployment = record.deployment_id,
                image = record.image,
                revision = record.revision,
            ),
            1,
        );
    }
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
