use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Context;
use jiji_config::Environment;
use jiji_ssh::SshSession;

/// Parses `.env` file content, mirroring the original tool's exact rules: comment/blank lines
/// skipped, key validated against `^[A-Za-z_][A-Za-z0-9_]*$` (an invalid key such as one produced
/// by an `export FOO=bar` line silently drops the whole line -- there is no `export` prefix
/// support), quoted values kept verbatim (including multi-line quoted values) with no escape
/// processing, unquoted values truncated at a literal `" #"` inline comment. Last duplicate key
/// wins.
pub fn parse_env_file(content: &str) -> BTreeMap<String, String> {
    let lines: Vec<&str> = content.split('\n').collect();
    let mut result = BTreeMap::new();
    let mut index = 0;

    while index < lines.len() {
        let raw_line = lines[index];
        let trimmed = raw_line.trim();
        index += 1;

        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some(eq_pos) = trimmed.find('=') else {
            continue;
        };
        let key = trimmed[..eq_pos].trim();
        if !is_valid_env_var_name(key) {
            continue;
        }

        let value_part = trimmed[eq_pos + 1..].trim();
        let value = match value_part.chars().next() {
            Some(quote) if quote == '"' || quote == '\'' => {
                let rest = &value_part[quote.len_utf8()..];
                match rest.find(quote) {
                    Some(close) => rest[..close].to_string(),
                    None => {
                        let mut parts = vec![rest.to_string()];
                        while index < lines.len() {
                            let next_raw = lines[index];
                            index += 1;
                            if let Some(close) = next_raw.find(quote) {
                                parts.push(next_raw[..close].to_string());
                                break;
                            }
                            parts.push(next_raw.to_string());
                        }
                        parts.join("\n")
                    }
                }
            }
            _ => match value_part.find(" #") {
                Some(comment_at) => value_part[..comment_at].trim().to_string(),
                None => value_part.to_string(),
            },
        };

        result.insert(key.to_string(), value);
    }

    result
}

fn is_valid_env_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `[custom.{env}, custom]` when `custom_path` is set, else `[.env.{env}, .env]`. The
/// environment-specific candidate is only included when `environment` is `Some`.
pub fn env_file_search_paths(
    project_root: &Path,
    environment: Option<&str>,
    custom_path: Option<&str>,
) -> Vec<PathBuf> {
    let base = custom_path.unwrap_or(".env");
    let mut paths = Vec::new();
    if let Some(env) = environment {
        paths.push(project_root.join(format!("{base}.{env}")));
    }
    paths.push(project_root.join(base));
    paths
}

/// Returns the first existing search-path file's parsed contents (no merging across files); a
/// missing file at every search path is not an error, it resolves to an empty map.
pub fn load_env_file(
    project_root: &Path,
    environment: Option<&str>,
    custom_path: Option<&str>,
) -> anyhow::Result<(BTreeMap<String, String>, Option<PathBuf>)> {
    for path in env_file_search_paths(project_root, environment, custom_path) {
        if path.is_file() {
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("Could not read env file {}", path.display()))?;
            return Ok((parse_env_file(&content), Some(path)));
        }
    }
    Ok((BTreeMap::new(), None))
}

/// `config_path` is assumed to be `<project_root>/.jiji/<file>.yml`.
pub fn project_root_from_config_path(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Merges a service's environment on top of the project-shared one: `clear` values from the
/// service override the shared ones on key conflict; `secrets` lists are unioned and deduplicated.
pub fn merge_environment(shared: &Environment, service: &Environment) -> Environment {
    let mut clear = shared.clear.clone();
    for (key, value) in &service.clear {
        clear.insert(key.clone(), value.clone());
    }

    let mut secrets = shared.secrets.clone();
    for secret in &service.secrets {
        if !secrets.contains(secret) {
            secrets.push(secret.clone());
        }
    }

    Environment { clear, secrets }
}

#[derive(Debug, Clone, Default)]
pub struct ResolvedEnvironment {
    pub values: BTreeMap<String, String>,
    pub secret_keys: BTreeSet<String>,
}

/// `clear` values are used as-is; `secrets` are resolved from `loaded` (the parsed `.env` map),
/// falling back to the host environment only when `allow_host_env` is set. Any secret resolved
/// from neither source is a hard error that lists every missing name, not just the first.
pub fn resolve_environment(
    env: &Environment,
    loaded: &BTreeMap<String, String>,
    allow_host_env: bool,
) -> anyhow::Result<ResolvedEnvironment> {
    let mut values = BTreeMap::new();
    for (key, value) in &env.clear {
        values.insert(key.clone(), value.to_string());
    }

    let mut secret_keys = BTreeSet::new();
    let mut missing = Vec::new();
    for name in &env.secrets {
        if let Some(value) = resolve_secret_name(name, loaded, allow_host_env) {
            values.insert(name.clone(), value);
            secret_keys.insert(name.clone());
        } else {
            missing.push(name.clone());
        }
    }

    if !missing.is_empty() {
        anyhow::bail!(
            "Missing required secrets: {}. Create a .env file with these secrets, or pass --host-env to read from the host environment.",
            missing.join(", ")
        );
    }

    Ok(ResolvedEnvironment {
        values,
        secret_keys,
    })
}

pub fn resolve_secret_name(
    name: &str,
    loaded: &BTreeMap<String, String>,
    allow_host_env: bool,
) -> Option<String> {
    loaded
        .get(name)
        .cloned()
        .or_else(|| allow_host_env.then(|| std::env::var(name).ok()).flatten())
}

pub fn is_bare_all_caps_name(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_uppercase())
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// A config value can opt into "run this local command and use its output" by wrapping it as
/// `$(...)`, the same convention shell command substitution uses. Returns the inner command on a
/// match, e.g. `is_command_expression("$(aws ecr get-login-password)")` ->
/// `Some("aws ecr get-login-password")`.
pub fn is_command_expression(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    trimmed
        .strip_prefix("$(")
        .and_then(|rest| rest.strip_suffix(')'))
}

const COMMAND_VALUE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Runs `command` through a shell locally (never on a remote host) and returns its trimmed
/// stdout, matching shell `$(...)` substitution semantics. `deploy.yml` is already a trusted,
/// version-controlled file -- this is not a new trust boundary, the same as `healthcheck.cmd`
/// already executing a configured command (inside a container, in that case).
pub async fn resolve_command_value(command: &str) -> anyhow::Result<String> {
    let result = crate::local_exec::run_captured_with_timeout(
        "sh",
        &["-c".to_string(), command.to_string()],
        None,
        None,
        Some(COMMAND_VALUE_TIMEOUT),
    )
    .await?;
    if !result.success {
        anyhow::bail!("Command '{command}' failed: {}", result.stderr.trim());
    }
    Ok(result.stdout.trim_end_matches('\n').to_string())
}

/// Renders `KEY=value` for clear vars and `KEY=<redacted>` for secrets -- safe to print in debug
/// output or audit logs.
pub fn redacted_summary(resolved: &ResolvedEnvironment) -> Vec<String> {
    resolved
        .values
        .iter()
        .map(|(key, value)| {
            if resolved.secret_keys.contains(key) {
                format!("{key}=<redacted>")
            } else {
                format!("{key}={value}")
            }
        })
        .collect()
}

/// Root of the per-project host-side staging tree (`env/`, and `mounts.rs`'s `files/`/
/// `directories/`). Intentionally a *relative* path: it resolves against the SSH login's default
/// working directory (its home directory), matching `stage_env_file` and
/// `mounts::remote_mount_base`'s own convention. `crate::commands::server::teardown` removes this
/// whole tree, since it's deploy-generated staging data (including secrets in `stage_env_file`'s
/// case), not user-facing persistent storage.
pub fn project_staging_dir(project: &str) -> String {
    format!(".jiji/{project}")
}

/// Writes `.jiji/{project}/env/{service}-{server}.env` (mode 0600) on the remote host via the
/// same stdin-pipe pattern `network/setup.rs::write_staged_file` uses, so no secret value is ever
/// embedded in a command string. Rejects any value containing a literal newline, since the
/// `--env-file` format has no quoting/escaping to represent one safely.
pub async fn stage_env_file(
    session: &SshSession,
    project: &str,
    service: &str,
    server: &str,
    resolved: &ResolvedEnvironment,
) -> anyhow::Result<String> {
    let mut content = String::new();
    for (key, value) in &resolved.values {
        if value.contains('\n') {
            anyhow::bail!(
                "Environment variable '{key}' for service '{service}' contains a newline, which the container engine's --env-file format cannot represent."
            );
        }
        content.push_str(key);
        content.push('=');
        content.push_str(value);
        content.push('\n');
    }

    let path = format!(".jiji/{project}/env/{service}-{server}.env");
    let temp = format!("{path}.jiji-new");
    let command = format!("set -eu; install -D -m 0600 /dev/stdin {temp}; mv {temp} {path}");
    let result = session
        .execute_with_input(&command, content.as_bytes())
        .await?;
    if !result.success {
        anyhow::bail!(
            "Could not stage environment file on {}: {}",
            session.host(),
            result.stderr.trim()
        );
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_blank_and_comment_lines() {
        let parsed = parse_env_file("\n# a comment\nFOO=bar\n");
        assert_eq!(parsed.get("FOO"), Some(&"bar".to_string()));
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn export_prefixed_lines_are_silently_skipped() {
        let parsed = parse_env_file("export FOO=bar\nBAR=baz\n");
        assert_eq!(parsed.get("FOO"), None);
        assert_eq!(parsed.get("BAR"), Some(&"baz".to_string()));
    }

    #[test]
    fn quoted_values_are_kept_verbatim_with_no_escape_processing() {
        let parsed = parse_env_file("FOO=\"bar\\nbaz\"\n");
        assert_eq!(parsed.get("FOO"), Some(&"bar\\nbaz".to_string()));
    }

    #[test]
    fn multiline_quoted_values_join_with_real_newlines() {
        let parsed = parse_env_file("FOO=\"line one\nline two\"\nBAR=1\n");
        assert_eq!(parsed.get("FOO"), Some(&"line one\nline two".to_string()));
        assert_eq!(parsed.get("BAR"), Some(&"1".to_string()));
    }

    #[test]
    fn unquoted_inline_comment_is_stripped_at_space_hash() {
        let parsed = parse_env_file("FOO=bar # a comment\nBAZ=no#hash\n");
        assert_eq!(parsed.get("FOO"), Some(&"bar".to_string()));
        // No preceding space before '#', so it is NOT treated as a comment start.
        assert_eq!(parsed.get("BAZ"), Some(&"no#hash".to_string()));
    }

    #[test]
    fn last_duplicate_key_wins() {
        let parsed = parse_env_file("FOO=first\nFOO=second\n");
        assert_eq!(parsed.get("FOO"), Some(&"second".to_string()));
    }

    #[test]
    fn search_paths_include_environment_variant_only_when_present() {
        let root = Path::new("/project");
        assert_eq!(
            env_file_search_paths(root, Some("staging"), None),
            vec![root.join(".env.staging"), root.join(".env")]
        );
        assert_eq!(
            env_file_search_paths(root, None, None),
            vec![root.join(".env")]
        );
        assert_eq!(
            env_file_search_paths(root, Some("staging"), Some("secrets/env")),
            vec![root.join("secrets/env.staging"), root.join("secrets/env")]
        );
    }

    #[test]
    fn missing_env_file_resolves_to_empty_map_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let (map, path) = load_env_file(dir.path(), None, None).unwrap();
        assert!(map.is_empty());
        assert_eq!(path, None);
    }

    #[test]
    fn resolve_environment_reports_every_missing_secret() {
        let env = Environment {
            clear: Default::default(),
            secrets: vec!["A".to_string(), "B".to_string()],
        };
        let err = resolve_environment(&env, &BTreeMap::new(), false).unwrap_err();
        assert!(err.to_string().contains('A'));
        assert!(err.to_string().contains('B'));
    }

    #[test]
    fn resolve_environment_falls_back_to_host_env_only_when_allowed() {
        std::env::set_var("JIJI_TEST_SECRET_ENV_RESOLUTION", "host-value");
        let env = Environment {
            clear: Default::default(),
            secrets: vec!["JIJI_TEST_SECRET_ENV_RESOLUTION".to_string()],
        };
        assert!(resolve_environment(&env, &BTreeMap::new(), false).is_err());
        let resolved = resolve_environment(&env, &BTreeMap::new(), true).unwrap();
        assert_eq!(
            resolved.values.get("JIJI_TEST_SECRET_ENV_RESOLUTION"),
            Some(&"host-value".to_string())
        );
        std::env::remove_var("JIJI_TEST_SECRET_ENV_RESOLUTION");
    }

    #[test]
    fn redaction_never_includes_a_secret_value() {
        let env = Environment {
            clear: [(
                "PLAIN".to_string(),
                jiji_config::ClearValue::String("visible".to_string()),
            )]
            .into_iter()
            .collect(),
            secrets: vec!["SECRET".to_string()],
        };
        let mut loaded = BTreeMap::new();
        loaded.insert("SECRET".to_string(), "top-secret".to_string());
        let resolved = resolve_environment(&env, &loaded, false).unwrap();
        let summary = redacted_summary(&resolved).join("\n");
        assert!(summary.contains("PLAIN=visible"));
        assert!(summary.contains("SECRET=<redacted>"));
        assert!(!summary.contains("top-secret"));
    }

    #[test]
    fn merge_environment_overrides_clear_and_unions_secrets() {
        let shared = Environment {
            clear: [(
                "A".to_string(),
                jiji_config::ClearValue::String("shared".to_string()),
            )]
            .into_iter()
            .collect(),
            secrets: vec!["S1".to_string()],
        };
        let service = Environment {
            clear: [(
                "A".to_string(),
                jiji_config::ClearValue::String("override".to_string()),
            )]
            .into_iter()
            .collect(),
            secrets: vec!["S1".to_string(), "S2".to_string()],
        };
        let merged = merge_environment(&shared, &service);
        assert_eq!(merged.clear.get("A").unwrap().to_string(), "override");
        assert_eq!(merged.secrets, vec!["S1".to_string(), "S2".to_string()]);
    }

    #[test]
    fn project_root_is_two_levels_above_config_file() {
        let path = Path::new("/srv/myproject/.jiji/deploy.yml");
        assert_eq!(
            project_root_from_config_path(path),
            Path::new("/srv/myproject")
        );
    }

    #[test]
    fn command_expression_detection_requires_dollar_paren_wrapper() {
        assert_eq!(
            is_command_expression("$(aws ecr get-login-password)"),
            Some("aws ecr get-login-password")
        );
        assert_eq!(is_command_expression("  $(echo hi)  "), Some("echo hi"));
        assert_eq!(is_command_expression("GITHUB_TOKEN"), None);
        assert_eq!(is_command_expression("literal-value"), None);
        assert_eq!(is_command_expression("$(unterminated"), None);
    }

    #[tokio::test]
    async fn command_value_resolution_trims_trailing_newline_and_reports_failures() {
        assert_eq!(resolve_command_value("echo hello").await.unwrap(), "hello");
        assert!(resolve_command_value("exit 1").await.is_err());
    }
}
