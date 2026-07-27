use jiji_config::{validate_config, RegistryType};
use jiji_tui::Ui;

use super::shared::{self, TargetKind, TargetOutcome};
use crate::registry;

pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    hosts: Option<&str>,
    services: Option<&str>,
    skip_local: bool,
    skip_remote: bool,
) -> anyhow::Result<()> {
    Ui::section("Registry Logout:");
    shared::validate_scope(services, skip_local, skip_remote)?;

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

    if config.builder.registry.kind == RegistryType::Local {
        Ui::say(
            "Registry: local (loopback-only, unauthenticated). No logout is required.",
            1,
        );
        Ui::success("Registry logout completed; the local registry needs no authentication.");
        return Ok(());
    }

    let server = registry::require_server(&config.builder.registry)?;
    Ui::say(&format!("Registry: {server}"), 1);
    Ui::say(&format!("Engine: {}", config.builder.engine), 1);

    // Resolve remote targets (and validate -H) before any mutation, so an unmatched filter or
    // missing `ssh:` section fails before the local logout runs.
    let servers = if skip_remote {
        Vec::new()
    } else {
        let selected = shared::select_target_servers(&config, hosts)?;
        if config.ssh.is_none() {
            anyhow::bail!(
                "No `ssh:` section configured in {}. Add at least `ssh.user:`, or pass --skip-remote.",
                path.display()
            );
        }
        selected
    };

    let mut outcomes = Vec::new();

    if skip_local {
        outcomes.push(TargetOutcome::Skipped(TargetKind::Local));
    } else {
        match registry::logout_local(config.builder.engine, &config.builder.registry).await {
            Ok(registry::AuthOutcome::LoggedOut) => {
                outcomes.push(TargetOutcome::Success(TargetKind::Local))
            }
            Ok(registry::AuthOutcome::AlreadyLoggedOut) => {
                outcomes.push(TargetOutcome::AlreadyDone(TargetKind::Local))
            }
            Err(error) => {
                outcomes.push(TargetOutcome::Failed(TargetKind::Local, error.to_string()))
            }
        }
    }

    if !skip_remote {
        let ssh = config
            .ssh
            .clone()
            .expect("presence checked above when !skip_remote");
        let (sessions, mut connect_failures) = shared::connect_all(&ssh, &servers).await?;
        outcomes.append(&mut connect_failures);

        Ui::section("Registry Logout:");
        for (name, session) in &sessions {
            match registry::logout_remote(session, config.builder.engine, &config.builder.registry)
                .await
            {
                Ok(registry::AuthOutcome::LoggedOut) => {
                    outcomes.push(TargetOutcome::Success(TargetKind::Remote(name.clone())))
                }
                Ok(registry::AuthOutcome::AlreadyLoggedOut) => {
                    outcomes.push(TargetOutcome::AlreadyDone(TargetKind::Remote(name.clone())))
                }
                Err(error) => outcomes.push(TargetOutcome::Failed(
                    TargetKind::Remote(name.clone()),
                    error.to_string(),
                )),
            }
        }
        shared::close_all(&sessions).await;
    }

    shared::report(
        "Registry logout",
        outcomes,
        "logged out",
        "already logged out",
    )
}
