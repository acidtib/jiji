use jiji_config::{
    build_config_path, get_available_configs, template_engine, validate_yaml, ConfigError, TEMPLATE,
};
use jiji_tui::Ui;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

pub fn run(environment: Option<&str>) -> anyhow::Result<()> {
    Ui::section("Configuration Initialization:");

    let config_path = build_config_path(environment);
    Ui::say(&format!("Target: {}", config_path.display()), 1);

    let existing = get_available_configs(Path::new("."));
    if !existing.is_empty() {
        Ui::say(&format!("Existing configurations ({}):", existing.len()), 1);
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
            Ui::say("Init command cancelled by user", 0);
            return Ok(());
        }
        Ui::say("Existing configuration will be replaced.", 1);
    }

    Ui::section("Creating Configuration:");
    Ui::say("Loading default configuration template", 1);
    let template = TEMPLATE;

    Ui::say("Writing configuration file", 1);
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&config_path, template)?;
    Ui::say(&format!("Created: {}", config_path.display()), 1);

    Ui::section("Validation:");
    Ui::say("Validating configuration", 1);
    let raw: serde_yaml::Value =
        serde_yaml::from_str(template).map_err(|source| ConfigError::Load {
            path: config_path.display().to_string(),
            source,
        })?;
    let result = validate_yaml(&raw);
    if result.valid {
        Ui::say("Configuration is valid", 1);
        if !result.warnings.is_empty() {
            Ui::say(&format!("Warnings ({}):", result.warnings.len()), 1);
            for w in &result.warnings {
                Ui::say(&format!("{}: {}", w.path, w.message), 2);
            }
        }
    } else {
        Ui::error(&format!(
            "Configuration validation failed with {} error(s):",
            result.errors.len()
        ));
        for e in &result.errors {
            Ui::say(&format!("{}: {}", e.path, e.message), 1);
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
        Ui::say(&format!("Checking {engine} availability"), 1);
        if engine_available(engine) {
            Ui::say(&format!("{engine} is available"), 1);
        } else {
            Ui::warn(&format!("{engine} is not available on this system"));
            Ui::say(
                &format!("Install {engine}, or select another engine in the configuration."),
                1,
            );
        }
    }

    Ui::section("Next Steps:");
    Ui::say("1. Review the generated configuration.", 1);
    Ui::say("2. Configure services, servers, and secrets.", 1);
    Ui::say("3. Run `jiji server setup` to prepare the servers.", 1);
    Ui::say("4. Run `jiji deploy`.", 1);

    Ui::success(&format!(
        "\nConfiguration file created: {}",
        config_path.display()
    ));

    Ok(())
}

/// Checks engine availability by running `{engine} --version` and checking the exit code —
/// matches the current Deno implementation (not `which`).
fn engine_available(engine: &str) -> bool {
    Command::new(engine)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}
