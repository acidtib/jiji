use crate::error::ConfigError;
use crate::schema::Config;
use std::fs;
use std::path::{Path, PathBuf};

const CONFIG_DIR: &str = ".jiji";
const CONFIG_EXTENSIONS: [&str; 2] = ["yml", "yaml"];
const DEFAULT_CONFIG_BASE: &str = "deploy";

/// Ordered candidate filenames (checked in this priority order, at every directory level).
fn build_config_filenames(environment: Option<&str>) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(env) = environment {
        for ext in CONFIG_EXTENSIONS {
            names.push(format!("{DEFAULT_CONFIG_BASE}.{env}.{ext}"));
        }
        for ext in CONFIG_EXTENSIONS {
            names.push(format!("{env}.{ext}"));
        }
    }
    for ext in CONFIG_EXTENSIONS {
        names.push(format!("{DEFAULT_CONFIG_BASE}.{ext}"));
    }
    names
}

/// The path `jiji init` writes to for a given environment.
pub fn build_config_path(environment: Option<&str>) -> PathBuf {
    let filename = match environment {
        Some(env) => format!("{DEFAULT_CONFIG_BASE}.{env}.yml"),
        None => format!("{DEFAULT_CONFIG_BASE}.yml"),
    };
    Path::new(CONFIG_DIR).join(filename)
}

/// Search upward from `start`, checking each candidate filename (in priority order) at every
/// directory level before climbing to the parent. Returns the first match.
pub fn find_config_file(environment: Option<&str>, start: &Path) -> Option<PathBuf> {
    let filenames = build_config_filenames(environment);
    let mut current = start.to_path_buf();
    loop {
        for filename in &filenames {
            let candidate = current.join(CONFIG_DIR).join(filename);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            None => break,
        }
    }
    None
}

fn build_config_not_found_message(environment: Option<&str>) -> String {
    let env_str = environment
        .map(|e| format!(" for environment '{e}'"))
        .unwrap_or_default();
    let example_files: Vec<String> = build_config_filenames(environment)
        .iter()
        .map(|f| format!("  - {CONFIG_DIR}/{f}"))
        .collect();
    format!(
        "No jiji configuration file found{env_str}. Please create one of the following files:\n{}",
        example_files.join("\n")
    )
}

/// Reads and parses a config file. Does not run semantic validation (see `crate::validation`).
pub fn load_from_file(path: &Path) -> Result<Config, ConfigError> {
    if !path.is_file() {
        return Err(ConfigError::FileNotFound(path.display().to_string()));
    }
    let content = fs::read_to_string(path)?;
    let value: serde_yaml::Value =
        serde_yaml::from_str(&content).map_err(|source| ConfigError::Load {
            path: path.display().to_string(),
            source,
        })?;
    if !value.is_mapping() {
        return Err(ConfigError::NotAnObject);
    }
    serde_yaml::from_value(value).map_err(|source| ConfigError::Load {
        path: path.display().to_string(),
        source,
    })
}

/// Loads config either from an explicit path or by searching upward from `start`.
pub fn load_config(
    environment: Option<&str>,
    config_path: Option<&Path>,
    start: &Path,
) -> Result<(Config, PathBuf), ConfigError> {
    let actual_path = match config_path {
        Some(p) => p.to_path_buf(),
        None => find_config_file(environment, start)
            .ok_or_else(|| ConfigError::NotFound(build_config_not_found_message(environment)))?,
    };
    let config = load_from_file(&actual_path)?;
    Ok((config, actual_path))
}

/// Lists `.jiji/*.{yml,yaml}` files directly under `search_path` (non-recursive: unlike
/// `find_config_file`, this does not climb parent directories). Missing/unreadable dir -> `[]`.
pub fn get_available_configs(search_path: &Path) -> Vec<PathBuf> {
    let config_dir = search_path.join(CONFIG_DIR);
    let mut configs = Vec::new();
    if let Ok(entries) = fs::read_dir(&config_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let is_config_file = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|name| {
                    CONFIG_EXTENSIONS
                        .iter()
                        .any(|ext| name.ends_with(&format!(".{ext}")))
                })
                .unwrap_or(false);
            if path.is_file() && is_config_file {
                configs.push(path);
            }
        }
    }
    configs.sort();
    configs
}
