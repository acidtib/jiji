use jiji_config::validate_config;
use jiji_tui::Ui;

use super::shared::{self, TargetKind, TargetOutcome};
use crate::{env_resolution, registry};

#[allow(clippy::too_many_arguments)]
pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    hosts: Option<&str>,
    services: Option<&str>,
    host_env: bool,
    skip_local: bool,
    skip_remote: bool,
) -> anyhow::Result<()> {
    Ui::section("Registry Login:");
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

    if config.builder.registry.is_local() {
        Ui::say(
            "Registry: local (loopback-only, unauthenticated). No login is required.",
            1,
        );
        Ui::success("Registry login completed; the local registry needs no authentication.");
        return Ok(());
    }

    let credentials = registry::require_login_credentials(&config.builder.registry)?;
    let raw_password = config.builder.registry.password.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "Registry login requires `builder.registry.password`. Configure a literal value or an ALL_CAPS secret name, then retry."
        )
    })?;

    Ui::say(&format!("Registry: {}", credentials.server), 1);
    Ui::say(&format!("Engine: {}", config.builder.engine), 1);

    // Resolve remote targets (and validate -H) before any mutation, so an unmatched filter or
    // missing `ssh:` section fails before the local login runs.
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

    let project_root = env_resolution::project_root_from_config_path(&path);
    let (loaded, loaded_from) =
        env_resolution::load_env_file(&project_root, environment, config.secrets_path.as_deref())?;
    if let Some(loaded_from) = loaded_from {
        Ui::say(
            &format!("Environment loaded from: {}", loaded_from.display()),
            1,
        );
    }
    let password = registry::resolve_registry_password(raw_password, &loaded, host_env).await?;

    let mut outcomes = Vec::new();

    if skip_local {
        outcomes.push(TargetOutcome::Skipped(TargetKind::Local));
    } else {
        match registry::login_local(config.builder.engine, &config.builder.registry, &password)
            .await
        {
            Ok(()) => outcomes.push(TargetOutcome::Success(TargetKind::Local)),
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

        Ui::section("Registry Login:");
        let hosts: Vec<String> = sessions.keys().cloned().collect();
        let progress =
            jiji_tui::ServerSetupProgress::with_title(hosts.clone(), "Logging in".to_string());
        let handle = progress.handle();
        for (name, session) in &sessions {
            handle.set_status(name, "logging in");
            match registry::login_remote(
                session,
                config.builder.engine,
                &config.builder.registry,
                &password,
            )
            .await
            {
                Ok(()) => {
                    handle.mark_success(name, "authenticated");
                    outcomes.push(TargetOutcome::Success(TargetKind::Remote(name.clone())))
                }
                Err(error) => {
                    handle.mark_failed(name, &error.to_string());
                    outcomes.push(TargetOutcome::Failed(
                        TargetKind::Remote(name.clone()),
                        error.to_string(),
                    ))
                }
            }
        }
        progress.finish();
        shared::close_all(&sessions).await;
    }

    shared::report("Registry login", outcomes, "authenticated", "authenticated")
}
