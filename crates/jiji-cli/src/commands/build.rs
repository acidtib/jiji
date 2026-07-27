use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use jiji_config::{load_config, validate_config, RegistryType};
use jiji_tui::Ui;

use crate::build_executor::{self, BuildExecutor};
use crate::{build_engine, build_plan, engine, env_resolution, registry, version_tag};

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
    let started_at = std::time::Instant::now();
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
    let executor_target = build_plan::select_executor(&config)?;
    let is_remote = matches!(executor_target, build_plan::ExecutorTarget::Remote { .. });
    let executor_identity = executor_target.identity();
    let git = version_tag::gather_git_status().await;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let (version, warning) = version_tag::resolve_version_tag(version, git.as_ref(), now);
    if let Some(warning) = warning {
        Ui::warn(&warning);
    }
    version_tag::validate_or_bail(&version)?;
    let plan = build_plan::compute_plan(&config, &config.project, &services, &version)?;
    Ui::say(&format!("Executor: {executor_identity}"), 1);
    Ui::say(&build_plan::render_plan_summary(&plan), 1);

    let push = !no_push;
    for entry in &plan {
        if let Some(error) = build_engine::multi_arch_requires_push(&entry.platforms, push) {
            anyhow::bail!("Service '{}': {error}", entry.service_name);
        }
    }

    let project_root = env_resolution::project_root_from_config_path(&path);
    let mut resolved_password = None;
    if push {
        match config.builder.registry.kind {
            RegistryType::Local => {
                Ui::section("Local Registry:");
                registry::ensure_local_registry(config.builder.engine, &config.builder.registry)
                    .await?;
            }
            RegistryType::Remote => {
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
                        resolved_password =
                            Some(registry::resolve_registry_password(raw, &loaded, host_env).await?);
                    }
                    _ => Ui::warn(
                        "Registry credentials are incomplete; skipping login. This is only safe for a public registry.",
                    ),
                }
            }
        }
    }

    let mut executor = BuildExecutor::prepare(
        executor_target,
        config.builder.engine,
        &config.project,
        &plan,
    )
    .await?;
    if let Some(engine::EngineStatus::Installed(version)) = executor.engine_status() {
        Ui::say(
            &format!(
                "{} {version} installed on {executor_identity}",
                config.builder.engine
            ),
            1,
        );
    }

    // Everything from here that can fail must still let `executor.finish()` run (staging
    // cleanup, tunnel cancellation, session close) -- so every failure is captured into
    // `run_result` instead of an early `?` return, and combined with the cleanup outcome once,
    // at the end, regardless of where in this sequence things went wrong.
    let mut run_result: anyhow::Result<()> = Ok(());
    if push {
        Ui::section("Registry:");
        run_result = executor
            .prepare_registry(
                config.builder.engine,
                &config.builder.registry,
                resolved_password.as_deref(),
            )
            .await;
        if run_result.is_ok() && is_remote {
            match config.builder.registry.kind {
                RegistryType::Local => Ui::say(
                    &format!("Tunneled local registry to {executor_identity}"),
                    1,
                ),
                RegistryType::Remote => {
                    if resolved_password.is_some() {
                        Ui::say(&format!("Logged in on {executor_identity}"), 1);
                    }
                }
            }
        }
    }

    if run_result.is_ok() {
        Ui::section("Building:");
        for entry in &plan {
            Ui::say(&entry.service_name, 1);
            run_result = build_plan::build_one(
                entry,
                &executor,
                config.builder.engine,
                no_cache,
                push,
                &config.project,
                &project_root,
                config.builder.registry.kind == RegistryType::Local,
            )
            .await
            .with_context(|| format!("Build failed for service '{}'", entry.service_name));
            if run_result.is_err() {
                break;
            }
        }
    }
    build_executor::combine_with_cleanup_error(run_result, executor.finish().await)?;

    Ui::section("Build Summary:");
    for entry in &plan {
        Ui::say(
            &format!(
                "{}: {}",
                entry.service_name,
                if !push {
                    if is_remote {
                        "built remotely, not pushed; the image remains only on the builder host"
                    } else {
                        "built locally"
                    }
                } else {
                    &entry.version_ref
                }
            ),
            1,
        );
    }
    Ui::success_elapsed("Build completed.", started_at.elapsed());
    Ok(())
}
