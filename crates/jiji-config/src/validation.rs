use crate::remote_builder::parse_remote_builder_uri;
use crate::schema::{Config, SshConfigFiles};

const MAX_SERVICES: usize = 500;
const MAX_REPLICAS: u32 = 2_000;
const MAX_NODES: usize = 32;

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
/// required top-level fields, each service has >=1 server, every listed server references a
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

    if config.servers.len() > MAX_NODES {
        errors.push(ValidationError {
            path: "servers".to_string(),
            message: format!(
                "A project supports at most {MAX_NODES} servers; {} are configured",
                config.servers.len()
            ),
            code: "TOO_MANY_SERVERS",
        });
    }
    if config.services.len() > MAX_SERVICES {
        errors.push(ValidationError {
            path: "services".to_string(),
            message: format!(
                "A project supports at most {MAX_SERVICES} services; {} are configured",
                config.services.len()
            ),
            code: "TOO_MANY_SERVICES",
        });
    }

    let mut total_replicas = 0_u32;
    for (name, service) in &config.services {
        total_replicas = total_replicas.saturating_add(service.replicas);
        if service.servers.is_empty() {
            errors.push(ValidationError {
                path: format!("services.{name}.servers"),
                message: format!(
                    "Service '{name}' must specify at least one server in 'servers' array"
                ),
                code: "NO_SERVERS",
            });
        }
        for host in &service.servers {
            if !config.servers.contains_key(host) {
                let mut available: Vec<&str> = config.servers.keys().map(String::as_str).collect();
                available.sort_unstable();
                errors.push(ValidationError {
                    path: format!("services.{name}.servers"),
                    message: format!(
                        "Server '{host}' not found in servers section. Available servers: {}",
                        available.join(", ")
                    ),
                    code: "UNDEFINED_SERVER",
                });
            }
        }
        if service.stop_first && service.replicas > 1 {
            errors.push(ValidationError {
                path: format!("services.{name}.replicas"),
                message: format!("Service '{name}' uses stop_first and must remain a singleton"),
                code: "STOP_FIRST_REQUIRES_SINGLETON",
            });
        }
        if service.network_mode.starts_with("container:") {
            errors.push(ValidationError {
                path: format!("services.{name}.network_mode"),
                message: format!(
                    "Service '{name}' uses unsupported container namespace networking"
                ),
                code: "UNSUPPORTED_NETWORK_MODE",
            });
        }
        if service.replicas > 1 && service.network_mode != "bridge" {
            errors.push(ValidationError {
                path: format!("services.{name}.network_mode"),
                message: format!("Service '{name}' can only scale with project bridge networking"),
                code: "NON_BRIDGE_SCALE",
            });
        }
        if service.network_mode != "bridge" && service.proxy.is_some() {
            errors.push(ValidationError {
                path: format!("services.{name}.proxy"),
                message: format!(
                    "Service '{name}' cannot use proxy ingress without project bridge networking"
                ),
                code: "NON_BRIDGE_PROXY",
            });
        }
        if let Some(upstream_name) = service.network_mode_dependency() {
            if upstream_name == name {
                errors.push(ValidationError {
                    path: format!("services.{name}.network_mode"),
                    message: format!(
                        "Service '{name}' cannot use network_mode: service:{name} to reference itself"
                    ),
                    code: "NETWORK_MODE_SERVICE_SELF_REFERENCE",
                });
            } else if let Some(upstream) = config.services.get(upstream_name) {
                if upstream.network_mode_dependency().is_some() {
                    errors.push(ValidationError {
                        path: format!("services.{name}.network_mode"),
                        message: format!(
                            "Service '{name}' cannot depend on '{upstream_name}', which is itself a network_mode:service dependent; chained namespace sharing is not supported"
                        ),
                        code: "NETWORK_MODE_SERVICE_CHAIN_UNSUPPORTED",
                    });
                }
                if !service
                    .servers
                    .iter()
                    .all(|host| upstream.servers.contains(host))
                {
                    errors.push(ValidationError {
                        path: format!("services.{name}.servers"),
                        message: format!(
                            "Service '{name}' shares '{upstream_name}''s network namespace, so its 'servers' must be a subset of '{upstream_name}''s servers"
                        ),
                        code: "NETWORK_MODE_SERVICE_SERVER_MISMATCH",
                    });
                }
            } else {
                let mut available: Vec<&str> = config.services.keys().map(String::as_str).collect();
                available.sort_unstable();
                errors.push(ValidationError {
                    path: format!("services.{name}.network_mode"),
                    message: format!(
                        "Service '{name}' references undefined service '{upstream_name}' in network_mode. Available services: {}",
                        available.join(", ")
                    ),
                    code: "UNDEFINED_NETWORK_MODE_SERVICE",
                });
            }
        }
        let has_local_state = !service.volumes.is_empty()
            || !service.files.is_empty()
            || !service.directories.is_empty();
        if service.replicas > 1 && has_local_state {
            errors.push(ValidationError {
                path: format!("services.{name}.replicas"),
                message: format!(
                    "Service '{name}' cannot scale local volumes, files, or directories implicitly"
                ),
                code: "STATEFUL_SCALE",
            });
        }
        if service.replicas > 1
            && (service.privileged || !service.devices.is_empty() || service.gpus.is_some())
        {
            errors.push(ValidationError {
                path: format!("services.{name}.replicas"),
                message: format!(
                    "Service '{name}' cannot scale exclusive host devices, GPUs, or privileged access"
                ),
                code: "EXCLUSIVE_RESOURCE_SCALE",
            });
        }
    }
    if total_replicas > MAX_REPLICAS {
        errors.push(ValidationError {
            path: "services".to_string(),
            message: format!(
                "A project supports at most {MAX_REPLICAS} logical replicas; {total_replicas} are configured"
            ),
            code: "TOO_MANY_REPLICAS",
        });
    }

    validate_builder(config, &mut errors);

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

fn validate_builder(config: &Config, errors: &mut Vec<ValidationError>) {
    let builder = &config.builder;
    if builder.local && builder.remote.is_some() {
        errors.push(ValidationError {
            path: "builder.remote".to_string(),
            message: "'builder.local: true' and 'builder.remote' cannot both be set. Choose one build target: set `builder.local: false` to build remotely, or remove `builder.remote` to build locally.".to_string(),
            code: "BUILDER_MODE_CONFLICT",
        });
        return;
    }

    if !builder.local && builder.remote.is_none() {
        errors.push(ValidationError {
            path: "builder.remote".to_string(),
            message: "'builder.local: false' requires `builder.remote` to be set to `ssh://[user@]hostname[:port]`.".to_string(),
            code: "BUILDER_REMOTE_REQUIRED",
        });
        return;
    }

    if let Some(remote) = &builder.remote {
        if let Err(error) = parse_remote_builder_uri(remote) {
            errors.push(ValidationError {
                path: "builder.remote".to_string(),
                message: error.to_string(),
                code: "INVALID_BUILDER_REMOTE",
            });
        }
    }
}
