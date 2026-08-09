use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use jiji_agent::api::{RequestBody, ResponseBody};
use jiji_config::validate_config;
use jiji_tui::Ui;

use super::select_single_cron;
use crate::commands::proxy::logs::{render_logs_command, stream_logs};

/// Requests an immediate run from the assigned agent (`CronRun`, Phase 2's agent API). A forbidden
/// overlap is reported as an actionable error naming the already-active run, not silently ignored
/// (the plan: "The command follows the same overlap rule as a scheduled run"). `--follow` polls
/// briefly for the container the agent starts in the background, then streams its logs.
pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    services: Option<&str>,
    cron: &str,
    follow: bool,
) -> anyhow::Result<()> {
    let start = std::env::current_dir()?;
    let (config, path) = crate::config_loading::load_config_for_ssh(
        environment,
        config_file.map(std::path::Path::new),
        &start,
    )
    .await?;
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

    let (service_name, _cron_config) = select_single_cron(&config, services, cron)?;
    let service = &config.services[service_name];

    let ssh = config.ssh.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "No `ssh:` section configured in {}. Add at least `ssh.user:` before running service cron run.",
            path.display()
        )
    })?;
    let (owner, resolved, newly_opened) =
        crate::cron_reconcile::find_owner(&ssh, &config, service_name, service, &BTreeMap::new())
            .await?;

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let response = crate::agent_client::call(
        &owner.session,
        &config.project,
        None,
        RequestBody::CronRun {
            service: service_name.to_string(),
            cron_name: cron.to_string(),
            timestamp,
        },
    )
    .await;

    let outcome = match response {
        Ok(ResponseBody::CronRunAccepted { run_id }) => Ok(run_id),
        Ok(ResponseBody::CronRunConflict { active_run_id }) => Err(anyhow::anyhow!(
            "Cron '{cron}' on service '{service_name}' is already running (run '{active_run_id}'); it was not started again."
        )),
        Ok(other) => Err(anyhow::anyhow!(
            "Agent on '{}' returned an unexpected response: {other:?}",
            owner.server_name
        )),
        Err(error) => Err(error),
    };
    let run_id = match outcome {
        Ok(run_id) => run_id,
        Err(error) => {
            crate::cron_reconcile::close_newly_opened(&resolved, &newly_opened).await;
            return Err(error);
        }
    };
    Ui::success(&format!(
        "Run accepted on '{}': {run_id}",
        owner.server_name
    ));

    if !follow {
        crate::cron_reconcile::close_newly_opened(&resolved, &newly_opened).await;
        return Ok(());
    }

    let result = follow_run(&owner.session, &config, &run_id).await;
    crate::cron_reconcile::close_newly_opened(&resolved, &newly_opened).await;
    result
}

/// Polls for the container the agent starts in the background (`CronRun` returns as soon as the
/// claim, not the run itself, succeeds -- see `cron_exec::execute_claimed_run`'s doc comment) up
/// to a bounded number of attempts, then streams its logs.
async fn follow_run(
    session: &jiji_ssh::SshSession,
    config: &jiji_config::Config,
    run_id: &str,
) -> anyhow::Result<()> {
    const MAX_ATTEMPTS: u32 = 30;
    let mut container_name = None;
    for _ in 0..MAX_ATTEMPTS {
        let runs = match crate::agent_client::call(
            session,
            &config.project,
            None,
            RequestBody::CronRuns {
                service: None,
                cron_name: None,
                run_id: Some(run_id.to_string()),
                since: None,
                limit: Some(1),
            },
        )
        .await?
        {
            ResponseBody::CronRuns { runs } => runs,
            other => anyhow::bail!("Agent returned an unexpected response: {other:?}"),
        };
        let Some(run) = runs.into_iter().next() else {
            anyhow::bail!("Run '{run_id}' disappeared before it could be followed.");
        };
        if let Some(name) = run.container_name {
            container_name = Some(name);
            break;
        }
        if !run.state.is_active() {
            anyhow::bail!(
                "Run '{run_id}' finished as {:?} before it ever started a container; nothing to follow.",
                run.state
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    let Some(container_name) = container_name else {
        anyhow::bail!(
            "Run '{run_id}' had not started a container after {MAX_ATTEMPTS} attempts; check `jiji service cron status` instead."
        );
    };

    let command = render_logs_command(
        config.builder.engine,
        &container_name,
        None,
        None,
        None,
        None,
        true,
    );
    stream_logs(session, &command).await
}
