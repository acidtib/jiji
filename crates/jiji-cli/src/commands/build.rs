use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use jiji_config::{load_config, validate_config};
use jiji_tui::Ui;

use crate::{build_engine, build_plan, env_resolution, registry, version_tag};

#[allow(clippy::too_many_arguments)]
pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    services: Option<&str>,
    version: Option<&str>,
    no_cache: bool,
    no_push: bool,
    host_env: bool,
) -> anyhow::Result<()> {
    Ui::section("Build:");
    let start = std::env::current_dir()?;
    let (config, path) = load_config(environment, config_file.map(std::path::Path::new), &start)?;
    Ui::say(&format!("Configuration loaded from: {}", path.display()), 1);
    let validation = validate_config(&config);
    if !validation.valid {
        for error in validation.errors {
            Ui::error(&format!("{}: {}", error.path, error.message));
        }
        anyhow::bail!("Configuration is invalid; fix the errors above and try again");
    }

    let filters: Vec<String> = services
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let services = build_plan::select_buildable_services(&config, &filters)?;
    build_plan::check_scope_guards(&config.builder)?;
    let git = version_tag::gather_git_status().await;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let (version, warning) = version_tag::resolve_version_tag(version, git.as_ref(), now);
    if let Some(warning) = warning {
        Ui::warn(&warning);
    }
    version_tag::validate_or_bail(&version)?;
    let plan = build_plan::compute_plan(&config, &config.project, &services, &version)?;
    Ui::say(&build_plan::render_plan_summary(&plan), 1);

    let push = !no_push;
    for entry in &plan {
        if let Some(error) = build_engine::multi_arch_requires_push(&entry.platforms, push) {
            anyhow::bail!("Service '{}': {error}", entry.service_name);
        }
    }

    let project_root = env_resolution::project_root_from_config_path(&path);
    if push {
        let (loaded, loaded_from) = env_resolution::load_env_file(
            &project_root,
            environment,
            config.secrets_path.as_deref(),
        )?;
        if let Some(path) = loaded_from {
            Ui::say(&format!("Environment loaded from: {}", path.display()), 1);
        }
        match (
            config.builder.registry.username.as_deref(),
            config.builder.registry.password.as_deref(),
        ) {
            (Some(_), Some(raw)) => {
                let password = registry::resolve_registry_password(raw, &loaded, host_env)?;
                registry::login_local(
                    config.builder.engine,
                    &config.builder.registry,
                    &password,
                )
                .await?;
            }
            _ => Ui::warn(
                "Registry credentials are incomplete; skipping login. This is only safe for a public registry.",
            ),
        }
    }

    Ui::section("Building:");
    for entry in &plan {
        Ui::say(&entry.service_name, 1);
        build_plan::build_one(
            entry,
            config.builder.engine,
            no_cache,
            push,
            &config.project,
            &project_root,
        )
        .await
        .with_context(|| format!("Build failed for service '{}'", entry.service_name))?;
    }
    Ui::section("Build Summary:");
    for entry in &plan {
        Ui::say(
            &format!(
                "{}: {}",
                entry.service_name,
                if push {
                    &entry.version_ref
                } else {
                    "built locally"
                }
            ),
            1,
        );
    }
    Ui::success("Build completed.");
    Ok(())
}
