use jiji_config::validate_config;
use jiji_tui::Ui;

use super::select_cron_services;

/// Reads purely from local configuration: no agent has an installed spec to compare against yet
/// (`jiji deploy` cron installation is a later phase), so every configured job is honestly
/// reported as `not deployed` rather than guessed at.
pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
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

    Ui::section("Service Cron Jobs:");
    let rows = select_cron_services(&config, services);
    if rows.is_empty() {
        Ui::say("No cron jobs are configured for the selected services.", 1);
        return Ok(());
    }
    for (service_name, cron, cron_name) in rows {
        Ui::say(
            &format!(
                "{service_name} {cron_name}: schedule=\"{schedule}\" timezone={timezone} state=not-deployed",
                schedule = cron.schedule,
                timezone = cron.timezone,
            ),
            1,
        );
    }
    Ok(())
}
