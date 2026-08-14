use jiji_config::validate_config;
use jiji_network::NetworkPlanner;
use jiji_ssh::{SshPool, SshSession};
use jiji_tui::Ui;

use crate::audit::{self, AuditStatus};
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

    let host_names: Vec<String> = selected.iter().map(|s| s.name.clone()).collect();
    Ui::say(
        &format!(
            "Targeting {} server(s): {}",
            host_names.len(),
            host_names.join(", ")
        ),
        1,
    );

    let started_at = std::time::Instant::now();
    let progress = Ui::proxy_restart_progress(host_names.clone());
    let handle = progress.handle();
    for name in &host_names {
        handle.set_status(name, "queued");
    }

    let project = config.project.clone();
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
        let handle = handle.clone();
        let project = project.clone();
        operations.push(move || async move {
            let host_started = std::time::Instant::now();
            handle.set_status(&name, "connecting");
            let result = async {
                let session = SshSession::connect(&options).await?;
                handle.set_status(&name, "restarting");
                let outcome = proxy::ensure_proxy(&session, engine, network, true).await;
                // No `HostGlobalProxy` lock is taken for this command today (a pre-existing
                // gap, not introduced here), so this entry gets no `lock_scope`, the same as
                // `service prune`'s unlocked audit entries.
                let (audit_status, audit_message) = match &outcome {
                    Ok(_) => (AuditStatus::Success, "jiji-proxy restarted".to_string()),
                    Err(error) => (AuditStatus::Failed, error.to_string()),
                };
                audit::record(
                    &session,
                    &project,
                    "proxy_restart",
                    audit_status,
                    audit_message,
                    None,
                    None,
                    Some(host_started.elapsed()),
                )
                .await;
                session.close().await;
                outcome
            }
            .await;
            match &result {
                Ok(_) => handle.mark_success(&name, "restarted"),
                Err(e) => handle.mark_failed(&name, &e.to_string()),
            }
            (name, result)
        });
    }

    let pool = SshPool::new(ssh.max_concurrent_starts as usize);
    let outcomes = pool.execute_concurrent(operations).await;
    progress.finish();

    let mut failures = Vec::new();
    let mut successes = 0usize;
    for (name, outcome) in outcomes {
        match outcome {
            Ok(_) => {
                Ui::result_ok(&name, "jiji-proxy restarted");
                successes += 1;
            }
            Err(error) => {
                Ui::result_error(&name, &error.to_string());
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
    Ui::success_elapsed(
        &format!("Restarted jiji-proxy on {successes} server(s)."),
        started_at.elapsed(),
    );
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
