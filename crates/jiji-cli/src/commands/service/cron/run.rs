use jiji_config::validate_config;
use jiji_tui::Ui;

use super::select_single_cron;

/// A manual run is requested from the job's assigned agent (`CronRun` in the agent API, a later
/// phase); no agent accepts that request yet, so this validates its target (one service, a real
/// cron name) and then reports the gap plainly rather than pretending to start a container.
pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    services: Option<&str>,
    cron: &str,
    _follow: bool,
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

    anyhow::bail!(
        "`jiji service cron run` is not implemented yet: no agent accepts a manual run of '{cron}' on service '{service_name}' in this release. See plans/service-cron.md."
    )
}
