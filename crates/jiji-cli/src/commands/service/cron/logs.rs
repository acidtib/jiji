use jiji_config::validate_config;
use jiji_tui::Ui;

use super::select_single_cron;

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

/// The agent stores run metadata and the container engine stores command output (see
/// `plans/service-cron.md`'s "Agent API"/"Durable Storage" sections); neither exists in this
/// release, so this validates its target (one service, a real cron name) and then reports the
/// gap plainly rather than pretending to stream logs that were never produced.
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
    if lines.is_some() || since.is_some() {
        Ui::say(
            "Note: --lines/--since are accepted but have no effect until this command is implemented.",
            1,
        );
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

    anyhow::bail!(
        "`jiji service cron logs` is not implemented yet: no run of '{cron}' on service '{service_name}' can exist until the scheduler and agent API land. See plans/service-cron.md."
    )
}
