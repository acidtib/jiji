use crate::schema::{Config, SshConfigFiles};

#[derive(Debug, Clone)]
pub struct ValidationError {
    pub path: String,
    pub message: String,
    pub code: &'static str,
}

#[derive(Debug, Clone)]
pub struct ValidationWarning {
    pub path: String,
    pub message: String,
    pub code: &'static str,
}

#[derive(Debug, Clone, Default)]
pub struct ValidationResult {
    pub valid: bool,
    pub errors: Vec<ValidationError>,
    pub warnings: Vec<ValidationWarning>,
}

const REQUIRED_TOP_LEVEL: [&str; 4] = ["project", "builder", "servers", "services"];

/// Validates raw YAML + the typed config. Scope for this slice (see design doc Non-Goals):
/// required top-level fields, each service has >=1 host, every service host references a
/// defined server, and basic SSH shape (port range, presence of `user` when `ssh:` is set).
/// Everything else (proxy/healthcheck rules, port/volume format, registry reachability,
/// project/server name patterns, host-consistency warnings, ...) is deferred to a future slice.
pub fn validate_yaml(raw: &serde_yaml::Value) -> ValidationResult {
    let mapping = match raw.as_mapping() {
        Some(m) => m,
        None => {
            return ValidationResult {
                valid: false,
                errors: vec![ValidationError {
                    path: String::new(),
                    message: "Configuration file must contain a valid YAML object".to_string(),
                    code: "NOT_AN_OBJECT",
                }],
                warnings: Vec::new(),
            };
        }
    };

    let mut errors = Vec::new();
    for key in REQUIRED_TOP_LEVEL {
        if !mapping.contains_key(serde_yaml::Value::String(key.to_string())) {
            errors.push(ValidationError {
                path: key.to_string(),
                message: format!("Missing required configuration: '{key}'"),
                code: "MISSING_FIELD",
            });
        }
    }
    if !errors.is_empty() {
        return ValidationResult {
            valid: false,
            errors,
            warnings: Vec::new(),
        };
    }

    match serde_yaml::from_value::<Config>(raw.clone()) {
        Ok(config) => validate_config(&config),
        Err(e) => ValidationResult {
            valid: false,
            errors: vec![ValidationError {
                path: String::new(),
                message: e.to_string(),
                code: "PARSE_ERROR",
            }],
            warnings: Vec::new(),
        },
    }
}

/// Runs the same checks as `validate_yaml` against an already-parsed `Config`.
pub fn validate_config(config: &Config) -> ValidationResult {
    let mut errors = Vec::new();
    let warnings = Vec::new();

    for (name, service) in &config.services {
        if service.hosts.is_empty() {
            errors.push(ValidationError {
                path: format!("services.{name}.hosts"),
                message: format!(
                    "Service '{name}' must specify at least one server in 'hosts' array"
                ),
                code: "NO_HOSTS",
            });
        }
        for host in &service.hosts {
            if !config.servers.contains_key(host) {
                let mut available: Vec<&str> = config.servers.keys().map(String::as_str).collect();
                available.sort_unstable();
                errors.push(ValidationError {
                    path: format!("services.{name}.hosts"),
                    message: format!(
                        "Server '{host}' not found in servers section. Available servers: {}",
                        available.join(", ")
                    ),
                    code: "UNDEFINED_SERVER",
                });
            }
        }
    }

    if let Some(ssh) = &config.ssh {
        let user_can_come_from_config = !matches!(ssh.config, SshConfigFiles::Enabled(false));
        let every_server_has_user = config.servers.values().all(|server| {
            server
                .user
                .as_deref()
                .is_some_and(|user| !user.trim().is_empty())
        });
        if ssh
            .user
            .as_deref()
            .is_none_or(|user| user.trim().is_empty())
            && !user_can_come_from_config
            && !every_server_has_user
        {
            errors.push(ValidationError {
                path: "ssh.user".to_string(),
                message: "Missing SSH user. Set `ssh.user`, set `user` on every server, or enable `ssh.config` with matching `User` entries.".to_string(),
                code: "MISSING_FIELD",
            });
        }
        if ssh.port == 0 {
            errors.push(ValidationError {
                path: "ssh.port".to_string(),
                message: "'port' in ssh must be a valid port number (1-65535)".to_string(),
                code: "INVALID_PORT",
            });
        }
    }

    ValidationResult {
        valid: errors.is_empty(),
        errors,
        warnings,
    }
}
