use std::collections::BTreeMap;

use jiji_agent::api::{RequestBody, ResponseBody};
use jiji_agent::cron::CronRunState;
use jiji_config::validate_config;
use jiji_tui::Ui;

use super::select_single_cron;
use crate::commands::proxy::logs::{effective_lines, render_logs_command, stream_logs};

pub struct LogsOptions<'a> {
    pub environment: Option<&'a str>,
    pub config_file: Option<&'a str>,
    pub services: Option<&'a str>,
    pub cron: &'a str,
    pub run: Option<&'a str>,
    pub lines: Option<u32>,
    pub since: Option<&'a str>,
    pub follow: bool,
}

/// The agent stores run metadata (`CronRuns`, Phase 2's agent API); the container engine stores
/// the actual command output (the plan's "jiji service cron logs" section: "The agent stores run
/// metadata, but the container engine stores command output"). This reads the target run's
/// `container_name` from the former, then reads/streams logs from the latter over SSH, exactly
/// like `jiji service logs`.
pub async fn run(options: LogsOptions<'_>) -> anyhow::Result<()> {
    let LogsOptions {
        environment,
        config_file,
        services,
        cron,
        run: run_id,
        lines,
        since,
        follow,
    } = options;

    if follow && run_id.is_some() {
        anyhow::bail!("--follow reads the active run; it cannot be combined with --run <id>");
    }

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
            "No `ssh:` section configured in {}. Add at least `ssh.user:` before running service cron logs.",
            path.display()
        )
    })?;
    let (owner, resolved, newly_opened) =
        crate::cron_reconcile::find_owner(&ssh, &config, service_name, service, &BTreeMap::new())
            .await?;

    let result = run_with_owner(
        &owner.session,
        &config,
        &owner.server_name,
        service_name,
        cron,
        run_id,
        lines,
        since,
        follow,
    )
    .await;
    crate::cron_reconcile::close_newly_opened(&resolved, &newly_opened).await;
    result
}

#[allow(clippy::too_many_arguments)]
async fn run_with_owner(
    session: &jiji_ssh::SshSession,
    config: &jiji_config::Config,
    owner_server_name: &str,
    service_name: &str,
    cron_name: &str,
    run_id: Option<&str>,
    lines: Option<u32>,
    since: Option<&str>,
    follow: bool,
) -> anyhow::Result<()> {
    let runs = match crate::agent_client::call(
        session,
        &config.project,
        None,
        RequestBody::CronRuns {
            service: Some(service_name.to_string()),
            cron_name: Some(cron_name.to_string()),
            run_id: run_id.map(str::to_string),
            since: None,
            limit: Some(1),
        },
    )
    .await?
    {
        ResponseBody::CronRuns { runs } => runs,
        other => anyhow::bail!(
            "Agent on '{owner_server_name}' returned an unexpected response: {other:?}"
        ),
    };
    let Some(target) = runs.into_iter().next() else {
        anyhow::bail!(
            "No run found for cron '{cron_name}' on service '{service_name}' on '{owner_server_name}'{}.",
            run_id
                .map(|id| format!(" matching run id '{id}'"))
                .unwrap_or_default()
        );
    };

    if follow && !target.state.is_active() {
        anyhow::bail!(
            "--follow requires an active run; the latest run for '{cron_name}' on service '{service_name}' is already {:?}.",
            target.state
        );
    }
    let Some(container_name) = target.container_name else {
        anyhow::bail!(
            "Run '{}' for cron '{cron_name}' on service '{service_name}' has no container ({}); there is nothing to show.",
            target.run_id,
            match target.state {
                CronRunState::Claimed => "it has not started yet",
                CronRunState::Failed => "it failed before starting",
                _ => "no container was recorded",
            }
        );
    };

    let effective_lines = effective_lines(lines, since, None);
    let command = render_logs_command(
        config.builder.engine,
        &container_name,
        effective_lines,
        since,
        None,
        None,
        follow,
    );
    if follow {
        stream_logs(session, &command).await
    } else {
        let result = session.execute(&command).await?;
        print!("{}", result.stdout);
        if !result.stderr.is_empty() {
            eprint!("{}", result.stderr);
        }
        if !result.success {
            anyhow::bail!("Remote logs command exited with status {:?}", result.code);
        }
        Ok(())
    }
}
