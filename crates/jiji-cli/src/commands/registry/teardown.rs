use jiji_config::{load_config, validate_config};
use jiji_tui::Ui;

use crate::registry;

pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    yes: bool,
    dry_run: bool,
) -> anyhow::Result<()> {
    Ui::section("Registry Teardown:");
    let start = std::env::current_dir()?;
    let (config, _) = load_config(environment, config_file.map(std::path::Path::new), &start)?;
    let validation = validate_config(&config);
    if !validation.valid {
        for error in validation.errors {
            Ui::error(&format!("{}: {}", error.path, error.message));
        }
        anyhow::bail!("Configuration is invalid; fix the errors above and try again");
    }
    if !config.builder.registry.is_local() {
        anyhow::bail!(
            "This project uses a remote registry. Remove `builder.registry.server` only if you intend to manage a local Jiji registry."
        );
    }

    Ui::say(
        &format!(
            "Container: {} ({}, port {})",
            registry::LOCAL_REGISTRY_NAME,
            config.builder.engine,
            config.builder.registry.port
        ),
        1,
    );

    let state =
        registry::local_registry_state(config.builder.engine, config.builder.registry.port).await?;
    if state.is_none() {
        Ui::success("Local registry is already absent.");
        return Ok(());
    }
    if dry_run {
        Ui::success("Dry run completed. No container was removed.");
        return Ok(());
    }
    if !yes
        && !Ui::confirm_typed(
            "Type the registry container name to confirm removal",
            registry::LOCAL_REGISTRY_NAME,
        )?
    {
        anyhow::bail!("Registry teardown cancelled; no container was removed.");
    }

    registry::remove_local_registry(config.builder.engine, config.builder.registry.port).await?;
    Ui::success("Local registry container removed.");
    Ok(())
}
