use jiji_config::validate_config;
use jiji_tui::Ui;

use super::select_cron_services;

/// Durable run state (last/next scheduled time, active run, skipped-overlap count) lives on each
/// job's assigned `jiji-agent` (`CronStatus` in the agent API, a later phase); there is nothing
/// for this command to read yet, so it validates its target and reports that plainly instead of
/// printing placeholder data.
pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    _hosts: Option<&str>,
    services: Option<&str>,
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

    let rows = select_cron_services(&config, services);
    if rows.is_empty() {
        anyhow::bail!(
            "No service with cron jobs matched the selected filter. Set -S to a service with a `crons:` map."
        );
    }

    anyhow::bail!(
        "`jiji service cron status` is not implemented yet: the jiji-agent scheduler and durable run state it would read from do not exist in this release. See plans/service-cron.md."
    )
}
