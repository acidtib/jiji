use std::path::Path;

use jiji_agent::api::{RequestBody, ResponseBody};
use jiji_config::validate_config;
use jiji_network::{NetworkPlanner, ServerPlan};
use jiji_ssh::SshSession;
use jiji_tui::Ui;

use crate::ssh_adapter;

pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    hosts: Option<&str>,
) -> anyhow::Result<()> {
    let start = std::env::current_dir()?;
    let (config, _) =
        crate::config_loading::load_config_for_ssh(environment, config_file.map(Path::new), &start)
            .await?;
    if !validate_config(&config).valid {
        anyhow::bail!("Configuration is invalid; fix it before compacting control-plane state");
    }
    let ssh = config
        .ssh
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No `ssh:` section is configured"))?;
    let plan = NetworkPlanner::new().plan(&config)?;
    let filters = hosts
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let selected: Vec<ServerPlan> = plan.select_hosts(&filters)?.into_iter().cloned().collect();
    if selected.is_empty() {
        anyhow::bail!("No servers are configured");
    }

    let selected_for_lock = selected.clone();
    let project_for_lock = config.project.clone();
    let servers_for_lock = config.servers.clone();
    super::backup::with_project_maintenance_lock(
        &project_for_lock,
        &servers_for_lock,
        ssh,
        &selected_for_lock,
        "jiji network compact".to_string(),
        move || async move {
            Ui::section("Compacting Control Plane:");
            let started = std::time::Instant::now();
            let hosts: Vec<String> = selected.iter().map(|s| s.name.clone()).collect();
            let progress =
                jiji_tui::ServerSetupProgress::with_title(hosts.clone(), "Compacting".to_string());
            let handle = progress.handle();
            let mut failures = Vec::new();
            for server_plan in &selected {
                handle.set_status(&server_plan.name, "compacting");
                let name = &server_plan.name;
                let options = ssh_adapter::connect_options(name, &config.servers[name], ssh)?;
                let session = match SshSession::connect(&options).await {
                    Ok(session) => session,
                    Err(error) => {
                        handle.mark_failed(name, &error.to_string());
                        failures.push(format!("{name}: {error}"));
                        continue;
                    }
                };
                let response = crate::agent_client::call(
                    &session,
                    &config.project,
                    None,
                    RequestBody::Compact,
                )
                .await;
                session.close().await;
                match response {
                    Ok(ResponseBody::Compacted {
                        membership_removed,
                        catalog_removed,
                        desired_removed,
                    }) => {
                        handle.mark_success(name, &format!("{catalog_removed} removed"));
                        Ui::result_ok(
                            name,
                            &format!(
                            "removed {membership_removed} membership, {catalog_removed} catalog, \
                     {desired_removed} desired operation(s)"
                        ),
                        )
                    }
                    Ok(other) => {
                        handle.mark_failed(name, "unexpected response");
                        failures.push(format!("{name}: unexpected response {other:?}"))
                    }
                    Err(error) => {
                        handle.mark_failed(name, &error.to_string());
                        failures.push(format!("{name}: {error}"))
                    }
                }
            }
            progress.finish();
            Ui::say(
                &format!(
                    "Compacted {} host(s) in {}",
                    hosts.len(),
                    jiji_tui::format_duration(started.elapsed())
                ),
                1,
            );
            for failure in &failures {
                Ui::result_warn("unavailable", failure);
            }
            if !failures.is_empty() {
                anyhow::bail!("Compaction failed on {} server(s)", failures.len());
            }
            Ui::success_elapsed("Control-plane compaction completed.", started.elapsed());
            Ok(())
        },
    )
    .await
}
