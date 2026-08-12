use jiji_config::{
    build_config_path, get_available_configs, template_engine, validate_yaml, ConfigError, TEMPLATE,
};
use jiji_tui::Ui;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

pub fn run(environment: Option<&str>) -> anyhow::Result<()> {
    let started = std::time::Instant::now();
    Ui::section("Init:");

    let config_path = build_config_path(environment);
    Ui::say(&format!("Target: {}", config_path.display()), 1);

    let existing = get_available_configs(Path::new("."));
    if !existing.is_empty() {
        // Keep compact — one line summary plus indented list, matches previous
        // behaviour but uses dimmed styling in TTY via Ui::say hierarchy.
        Ui::say(
            &format!("Existing: {} file(s) in .jiji/", existing.len()),
            1,
        );
        for cfg in &existing {
            Ui::say(&cfg.display().to_string(), 2);
        }
    }

    if config_path.exists() {
        Ui::warn(&format!(
            "Configuration already exists at {}",
            config_path.display()
        ));
        let overwrite = Ui::confirm(
            &format!(
                "Config file already exists at {}. Overwrite it?",
                config_path.display()
            ),
            false,
        )?;
        if !overwrite {
            Ui::say("Init cancelled — existing file kept.", 0);
            return Ok(());
        }
        Ui::say("Overwriting existing configuration.", 1);
    }

    // Single concise "Creating" block — the template is embedded, so there is no
    // real loading time to report step-by-step.
    let template = render_template();
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&config_path, &template)?;
    Ui::result_ok(
        &config_path.display().to_string(),
        &format!("created ({} bytes)", template.len()),
    );

    Ui::section("Validation:");
    let raw: serde_yaml::Value =
        serde_yaml::from_str(&template).map_err(|source| ConfigError::Load {
            path: config_path.display().to_string(),
            source,
        })?;
    let result = validate_yaml(&raw);
    if result.valid {
        // Keep exact phrase for test compatibility, but surface as a structured result.
        Ui::result_ok("configuration", "valid — Configuration is valid");
        if !result.warnings.is_empty() {
            for w in &result.warnings {
                Ui::result_warn(&w.path, &w.message);
            }
        }
    } else {
        Ui::error(&format!(
            "Configuration validation failed with {} error(s):",
            result.errors.len()
        ));
        for e in &result.errors {
            Ui::result_error(&e.path, &e.message);
        }
        let joined = result
            .errors
            .iter()
            .map(|e| format!("{}: {}", e.path, e.message))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(ConfigError::Invalid(joined).into());
    }

    if let Some(engine) = template_engine() {
        if engine_available(engine) {
            Ui::result_ok("engine", &format!("{engine} available"));
        } else {
            Ui::result_warn("engine", &format!("{engine} not found"));
            Ui::say(
                &format!(
                    "Install {engine} or change builder.engine in {}",
                    config_path.display()
                ),
                1,
            );
        }
    }

    Ui::section("Next steps:");
    // Keep numbered steps but with tighter phrasing; indentation matches Ui::say hierarchy.
    Ui::say(
        "1. Review .jiji/deploy.yml — set project, servers, and services.",
        1,
    );
    Ui::say(
        "2. Add secrets to .env / host env as referenced by the template.",
        1,
    );
    Ui::say(
        "3. Run `jiji server setup` to bootstrap the WireGuard mesh and jiji-agent.",
        1,
    );
    Ui::say("4. Run `jiji deploy` to ship the first deployment.", 1);
    Ui::say(
        &format!(
            "Template: {} (edit before deploying)",
            config_path.display()
        ),
        1,
    );

    Ui::success_elapsed(
        &format!("Configuration file created: {}", config_path.display()),
        started.elapsed(),
    );

    Ok(())
}

fn render_template() -> String {
    TEMPLATE.to_string()
}

/// Checks engine availability by running `{engine} --version` and checking the exit code.
fn engine_available(engine: &str) -> bool {
    Command::new(engine)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendered_template_leaves_networking_implicit() {
        let rendered = render_template();
        assert!(!rendered.contains("\nnetwork:\n"));
        assert!(rendered.contains("# network:\n"));
    }
}
