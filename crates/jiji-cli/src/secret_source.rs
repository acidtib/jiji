use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use jiji_config::{Config, SecretsAdapter, SecretsAdapterKind};
use tokio::sync::Mutex;

use crate::env_resolution;

const DOPPLER_TIMEOUT: Duration = Duration::from_secs(30);

/// Successful adapter fetches, keyed by adapter settings, so one process calls the adapter once.
static ADAPTER_CACHE: Mutex<BTreeMap<String, BTreeMap<String, String>>> =
    Mutex::const_new(BTreeMap::new());

#[derive(Debug, Clone)]
pub enum SecretSource {
    EnvFile(PathBuf),
    NoEnvFile,
    Adapter {
        settings: SecretsAdapter,
        fetched: bool,
    },
}

impl SecretSource {
    /// Where a missing name should be added, for use in error hints.
    pub fn location(&self) -> String {
        match self {
            SecretSource::EnvFile(_) | SecretSource::NoEnvFile => {
                "the selected .env file".to_string()
            }
            SecretSource::Adapter { settings, .. } => describe_adapter(settings),
        }
    }

    pub fn is_adapter(&self) -> bool {
        matches!(self, SecretSource::Adapter { .. })
    }

    /// The `... loaded from: ...` line commands print, or `None` when nothing was loaded.
    pub fn loaded_line(&self) -> Option<String> {
        match self {
            SecretSource::EnvFile(path) => {
                Some(format!("Environment loaded from: {}", path.display()))
            }
            SecretSource::NoEnvFile => None,
            SecretSource::Adapter { settings, fetched } => {
                fetched.then(|| format!("Secrets loaded from: {}", describe_adapter(settings)))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct LoadedSecrets {
    pub values: BTreeMap<String, String>,
    pub source: SecretSource,
}

impl LoadedSecrets {
    #[cfg(test)]
    pub fn from_env_map(values: BTreeMap<String, String>) -> Self {
        LoadedSecrets {
            values,
            source: SecretSource::NoEnvFile,
        }
    }

    pub fn say_loaded(&self) {
        if let Some(line) = self.source.loaded_line() {
            jiji_tui::Ui::say(&line, 1);
        }
    }
}

pub fn describe_adapter(settings: &SecretsAdapter) -> String {
    match settings.adapter {
        SecretsAdapterKind::Doppler => {
            let project = settings
                .project
                .as_deref()
                .map_or("from doppler setup".to_string(), |project| {
                    project.to_string()
                });
            let config = settings
                .config
                .as_deref()
                .map_or("from doppler setup".to_string(), |config| {
                    config.to_string()
                });
            format!("doppler (project {project}, config {config})")
        }
    }
}

/// Loads secret values for a command body. With an adapter configured, the adapter is called
/// only when the config references at least one secret.
pub async fn load_secrets(
    project_root: &Path,
    environment: Option<&str>,
    config: &Config,
) -> anyhow::Result<LoadedSecrets> {
    load_secrets_if(
        project_root,
        environment,
        config,
        references_any_secret(config),
    )
    .await
}

/// Same as [`load_secrets`], with the caller deciding whether an adapter fetch is needed.
/// `.env` files are always read when no adapter is configured.
pub async fn load_secrets_if(
    project_root: &Path,
    environment: Option<&str>,
    config: &Config,
    adapter_needed: bool,
) -> anyhow::Result<LoadedSecrets> {
    let Some(settings) = &config.secrets else {
        let (values, path) = env_resolution::load_env_file(
            project_root,
            environment,
            config.secrets_path.as_deref(),
        )?;
        let source = path.map_or(SecretSource::NoEnvFile, SecretSource::EnvFile);
        return Ok(LoadedSecrets { values, source });
    };
    if !adapter_needed {
        return Ok(LoadedSecrets {
            values: BTreeMap::new(),
            source: SecretSource::Adapter {
                settings: settings.clone(),
                fetched: false,
            },
        });
    }
    let values = fetch_cached(settings).await?;
    Ok(LoadedSecrets {
        values,
        source: SecretSource::Adapter {
            settings: settings.clone(),
            fetched: true,
        },
    })
}

/// True when any runtime-resolved field in `config` names a secret.
pub fn references_any_secret(config: &Config) -> bool {
    crate::commands::secrets::print::collect_secret_refs(config, &[])
        .iter()
        .any(|reference| reference.runtime_resolved)
}

/// True when a server host or SSH key entry is an ALL_CAPS reference.
pub fn references_ssh_secret(config: &Config) -> bool {
    let is_reference = |value: &String| env_resolution::is_bare_all_caps_name(value);
    config
        .servers
        .values()
        .any(|server| is_reference(&server.host) || server.keys.iter().flatten().any(is_reference))
        || config
            .ssh
            .as_ref()
            .is_some_and(|ssh| ssh.keys.iter().flatten().any(is_reference))
}

async fn fetch_cached(settings: &SecretsAdapter) -> anyhow::Result<BTreeMap<String, String>> {
    let key = format!(
        "{}\0{}\0{}",
        settings.adapter,
        settings.project.as_deref().unwrap_or_default(),
        settings.config.as_deref().unwrap_or_default()
    );
    let mut cache = ADAPTER_CACHE.lock().await;
    if let Some(values) = cache.get(&key) {
        return Ok(values.clone());
    }
    let values = fetch_from_adapter(settings).await?;
    cache.insert(key, values.clone());
    Ok(values)
}

/// One per adapter: returns the adapter's whole name-to-value map.
trait SecretsProvider {
    async fn fetch(&self) -> anyhow::Result<BTreeMap<String, String>>;
}

async fn fetch_from_adapter(settings: &SecretsAdapter) -> anyhow::Result<BTreeMap<String, String>> {
    match settings.adapter {
        SecretsAdapterKind::Doppler => {
            Doppler {
                settings: settings.clone(),
            }
            .fetch()
            .await
        }
    }
}

struct Doppler {
    settings: SecretsAdapter,
}

impl Doppler {
    fn args(&self) -> Vec<String> {
        let mut args: Vec<String> = ["secrets", "download", "--no-file", "--format", "json"]
            .map(str::to_string)
            .into();
        if let Some(project) = &self.settings.project {
            args.extend(["--project".to_string(), project.clone()]);
        }
        if let Some(config) = &self.settings.config {
            args.extend(["--config".to_string(), config.clone()]);
        }
        args
    }
}

impl SecretsProvider for Doppler {
    async fn fetch(&self) -> anyhow::Result<BTreeMap<String, String>> {
        let result = match crate::local_exec::run_captured_with_timeout(
            "doppler",
            &self.args(),
            None,
            None,
            Some(DOPPLER_TIMEOUT),
        )
        .await
        {
            Ok(result) => result,
            Err(error) => return Err(doppler_launch_error(error)),
        };
        if !result.success {
            anyhow::bail!(
                "The Doppler CLI failed (exit {}): {}. Run `doppler login` (or set DOPPLER_TOKEN), and check secrets.project and secrets.config in your jiji config.",
                result
                    .code
                    .map_or("killed by signal".to_string(), |code| code.to_string()),
                result.stderr.trim()
            );
        }
        parse_doppler_json(&result.stdout)
    }
}

fn doppler_launch_error(error: anyhow::Error) -> anyhow::Error {
    if error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
    }) {
        return anyhow::anyhow!(
            "The Doppler CLI (`doppler`) was not found on PATH. Install it (https://docs.doppler.com/docs/install-cli), or remove the secrets: block to read secrets from .env."
        );
    }
    if let Some(timeout) = error.downcast_ref::<crate::local_exec::CommandTimedOut>() {
        return anyhow::anyhow!(
            "The Doppler CLI did not respond within {}s. Check your network connection and that `doppler secrets` works in this shell, then retry.",
            timeout.seconds
        );
    }
    error.context("Could not run the Doppler CLI. Check that `doppler` is installed and executable")
}

/// Parses `doppler secrets download --format json` output. Errors never echo the input.
fn parse_doppler_json(stdout: &str) -> anyhow::Result<BTreeMap<String, String>> {
    let parsed: serde_json::Value = serde_json::from_str(stdout).context(
        "The Doppler CLI returned output that is not valid JSON. Run `doppler secrets download --no-file --format json` to check it, and update the Doppler CLI if needed",
    )?;
    let Some(object) = parsed.as_object() else {
        anyhow::bail!(
            "The Doppler CLI returned JSON that is not an object of secret names to values. Run `doppler secrets download --no-file --format json` to check it, and update the Doppler CLI if needed."
        );
    };
    let mut values = BTreeMap::new();
    for (name, value) in object {
        let Some(value) = value.as_str() else {
            anyhow::bail!(
                "Doppler secret '{name}' is not a string value. Store it as a string in Doppler."
            );
        };
        values.insert(name.clone(), value.to_string());
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(project: Option<&str>, config: Option<&str>) -> SecretsAdapter {
        SecretsAdapter {
            adapter: SecretsAdapterKind::Doppler,
            project: project.map(str::to_string),
            config: config.map(str::to_string),
        }
    }

    #[test]
    fn doppler_args_pass_project_and_config_only_when_set() {
        let base = ["secrets", "download", "--no-file", "--format", "json"];
        let both = Doppler {
            settings: settings(Some("app"), Some("prd")),
        };
        let mut expected: Vec<&str> = base.to_vec();
        expected.extend(["--project", "app", "--config", "prd"]);
        assert_eq!(both.args(), expected);

        let neither = Doppler {
            settings: settings(None, None),
        };
        assert_eq!(neither.args(), base);
    }

    #[test]
    fn parses_string_values() {
        let values = parse_doppler_json(r#"{"A":"1","B":"two"}"#).unwrap();
        assert_eq!(values["A"], "1");
        assert_eq!(values["B"], "two");
    }

    #[test]
    fn non_string_value_names_the_key_without_the_value() {
        let error = parse_doppler_json(r#"{"PORT":8080}"#)
            .unwrap_err()
            .to_string();
        assert!(error.contains("'PORT'"));
        assert!(!error.contains("8080"));
    }

    #[test]
    fn invalid_json_and_non_object_do_not_echo_output() {
        let error = format!("{:#}", parse_doppler_json("hunter2 not json").unwrap_err());
        assert!(error.contains("not valid JSON"));
        assert!(!error.contains("hunter2"));
        let error = parse_doppler_json(r#""hunter2""#).unwrap_err().to_string();
        assert!(error.contains("not an object"));
        assert!(!error.contains("hunter2"));
    }

    #[test]
    fn describes_adapter_scope() {
        assert_eq!(
            describe_adapter(&settings(Some("app"), Some("prd"))),
            "doppler (project app, config prd)"
        );
        assert_eq!(
            describe_adapter(&settings(None, None)),
            "doppler (project from doppler setup, config from doppler setup)"
        );
    }

    #[test]
    fn unfetched_adapter_prints_no_loaded_line() {
        let source = SecretSource::Adapter {
            settings: settings(Some("app"), None),
            fetched: false,
        };
        assert!(source.loaded_line().is_none());
        let source = SecretSource::Adapter {
            settings: settings(Some("app"), Some("prd")),
            fetched: true,
        };
        assert_eq!(
            source.loaded_line().unwrap(),
            "Secrets loaded from: doppler (project app, config prd)"
        );
    }
}
