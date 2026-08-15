use std::collections::HashSet;
use std::str::FromStr;

use crate::remote_builder::parse_remote_builder_uri;
use crate::schema::{BuildValue, CommandValue, Config, Service, SshConfigFiles, SslValue};

const MAX_SERVICES: usize = 500;
const MAX_REPLICAS: u32 = 2_000;
const MAX_NODES: usize = 32;
const MAX_CRONS_PER_SERVICE: usize = 32;
const MAX_CRONS_PER_PROJECT: usize = 1_000;

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
    let mut total_crons = 0_usize;
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
        if service.network_mode == "none" {
            errors.push(ValidationError {
                path: format!("services.{name}.network_mode"),
                message: format!(
                    "Service '{name}' uses network_mode: none, which is not supported: every service needs a reachable address for DNS and health checks. Use 'network_mode: service:<name>' to explicitly share another service's namespace, or 'crons:' for isolated one-off/scheduled work."
                ),
                code: "NETWORK_MODE_NONE_UNSUPPORTED",
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
        validate_service_crons(name, service, &mut errors);
        validate_service_build_secrets(name, service, &mut errors);
        total_crons = total_crons.saturating_add(service.crons.len());
    }
    if total_crons > MAX_CRONS_PER_PROJECT {
        errors.push(ValidationError {
            path: "services".to_string(),
            message: format!(
                "A project supports at most {MAX_CRONS_PER_PROJECT} cron jobs; {total_crons} are configured"
            ),
            code: "TOO_MANY_CRONS",
        });
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
    validate_proxy_hosts(config, &mut errors);
    validate_proxy_has_a_routable_host(config, &mut errors);
    validate_tcp_targets(config, &mut errors);
    validate_host_network_ports(config, &mut errors);

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
    if let Some(remote) = &builder.remote {
        if let Err(error) = parse_remote_builder_uri(remote) {
            errors.push(ValidationError {
                path: "builder.remote".to_string(),
                message: error.to_string(),
                code: "INVALID_BUILDER_REMOTE",
            });
        }
    }
    if builder.registry.server.is_none()
        && (builder.registry.username.is_some() || builder.registry.password.is_some())
    {
        errors.push(ValidationError {
            path: "builder.registry.server".to_string(),
            message: "Registry credentials require `builder.registry.server`; add the remote registry server or remove `username` and `password` to use Jiji's local registry".to_string(),
            code: "REGISTRY_CREDENTIALS_REQUIRE_SERVER",
        });
    }
}

/// Mirrors `env_resolution::is_bare_all_caps_name` in `jiji-cli`, duplicated here rather than
/// imported: the dependency graph only runs `jiji-cli` -> `jiji-config`, never the other way, so
/// `jiji-config` cannot call into `jiji-cli`. `[A-Z0-9_]`-only already excludes both `,` and `=`
/// as a side effect, so no separate "reject these characters" rule is needed alongside it.
fn is_bare_all_caps_name(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_uppercase())
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// `build.secrets` names double as both the `.env`/host-env lookup key and the `--secret id=`
/// value passed to the container engine, so they're held to the same ALL_CAPS shape
/// `environment.secrets` already requires, with no duplicates within one service.
fn validate_service_build_secrets(
    service_name: &str,
    service: &Service,
    errors: &mut Vec<ValidationError>,
) {
    let Some(BuildValue::Detailed(build)) = &service.build else {
        return;
    };
    let Some(secrets) = &build.secrets else {
        return;
    };
    let path = format!("services.{service_name}.build.secrets");
    let mut seen = HashSet::new();
    for name in secrets {
        if !is_bare_all_caps_name(name) {
            errors.push(ValidationError {
                path: path.clone(),
                message: format!(
                    "Service '{service_name}' build secret '{name}' must be an ALL_CAPS name (letters, digits, underscore, starting with a letter), matching environment.secrets' convention"
                ),
                code: "BUILD_SECRET_NAME_INVALID",
            });
            continue;
        }
        if !seen.insert(name.as_str()) {
            errors.push(ValidationError {
                path: path.clone(),
                message: format!(
                    "Service '{service_name}' build secret '{name}' is listed more than once in build.secrets"
                ),
                code: "BUILD_SECRET_DUPLICATE",
            });
        }
    }
}

/// jiji-proxy supports a single-label wildcard host (`*.example.com`,
/// matching `foo.example.com` but not `deep.foo.example.com` or the bare
/// `example.com`) but cannot obtain an ACME certificate for one: its
/// automation is HTTP-01 only, and only DNS-01 can issue a wildcard
/// certificate. A wildcard host may still use TLS via a user-supplied
/// static certificate (`ssl: { certificate_pem, private_key_pem }`).
fn validate_proxy_hosts(config: &Config, errors: &mut Vec<ValidationError>) {
    for (name, service) in &config.services {
        let Some(proxy) = &service.proxy else {
            continue;
        };
        if let Some(targets) = &proxy.targets {
            for (index, target) in targets.iter().enumerate() {
                validate_hosts_and_ssl(
                    name,
                    &format!("services.{name}.proxy.targets[{index}]"),
                    target.hosts.as_deref().unwrap_or_default(),
                    &target.ssl,
                    errors,
                );
            }
        } else {
            validate_hosts_and_ssl(
                name,
                &format!("services.{name}.proxy"),
                proxy.hosts.as_deref().unwrap_or_default(),
                &proxy.ssl,
                errors,
            );
        }
    }
}

/// An HTTP route (no `listen_port`) with no `hosts:` builds zero `RouteTarget`s
/// (`proxy_routes::targets_for_service` maps an empty/absent `hosts:` list to an empty `Vec`,
/// not an error) -- the service deploys, its container starts and passes its own health check,
/// but jiji-proxy never registers a route for it at all, so the URL silently never works and
/// nothing in `jiji deploy`'s own output says why (confirmed live). jiji-proxy routes by Host
/// header (see `validate_proxy_hosts`'s wildcard-host doc comment), so at least one host is
/// required for any HTTP route; a raw-TCP target (`listen_port` set) has no Host header at all
/// and is exempt.
fn validate_proxy_has_a_routable_host(config: &Config, errors: &mut Vec<ValidationError>) {
    for (name, service) in &config.services {
        let Some(proxy) = &service.proxy else {
            continue;
        };
        if let Some(targets) = &proxy.targets {
            for (index, target) in targets.iter().enumerate() {
                if target.listen_port.is_some() {
                    continue;
                }
                if target.hosts.as_deref().unwrap_or_default().is_empty() {
                    errors.push(ValidationError {
                        path: format!("services.{name}.proxy.targets[{index}].hosts"),
                        message: format!(
                            "Service '{name}' proxy target has no `hosts:` configured. jiji-proxy routes HTTP traffic by Host header, so this route can never be reached; add at least one hostname (or a wildcard like '*.example.com'), or set `listen_port` for a raw TCP route that doesn't need one."
                        ),
                        code: "PROXY_HTTP_ROUTE_WITHOUT_HOSTS",
                    });
                }
            }
        } else if proxy.listen_port.is_none()
            && proxy.port.is_some()
            && proxy.hosts.as_deref().unwrap_or_default().is_empty()
        {
            errors.push(ValidationError {
                path: format!("services.{name}.proxy.hosts"),
                message: format!(
                    "Service '{name}' proxy has no `hosts:` configured. jiji-proxy routes HTTP traffic by Host header, so this route can never be reached; add at least one hostname (or a wildcard like '*.example.com'), or set `listen_port` for a raw TCP route that doesn't need one."
                ),
                code: "PROXY_HTTP_ROUTE_WITHOUT_HOSTS",
            });
        }
    }
}

fn validate_hosts_and_ssl(
    service_name: &str,
    path: &str,
    hosts: &[String],
    ssl: &Option<SslValue>,
    errors: &mut Vec<ValidationError>,
) {
    for host in hosts {
        if !host.contains('*') {
            continue;
        }
        let well_formed =
            host.starts_with("*.") && !host[2..].is_empty() && !host[2..].contains('*');
        if !well_formed {
            errors.push(ValidationError {
                path: path.to_string(),
                message: format!(
                    "Service '{service_name}' proxy host '{host}' is not a valid wildcard pattern; only a single leading label may be '*', e.g. '*.example.com'"
                ),
                code: "PROXY_INVALID_WILDCARD_HOST",
            });
            continue;
        }
        if matches!(ssl, Some(SslValue::Enabled(true))) {
            errors.push(ValidationError {
                path: path.to_string(),
                message: format!(
                    "Service '{service_name}' proxy host '{host}' is a wildcard and cannot use 'ssl: true': jiji-proxy's ACME automation only supports HTTP-01 challenges, which cannot issue a wildcard certificate. Provide a static certificate instead with 'ssl: {{ certificate_pem, private_key_pem }}', or remove 'ssl' for this host."
                ),
                code: "PROXY_WILDCARD_REQUIRES_STATIC_CERT",
            });
        }
    }
}

/// A `listen_port` selects raw TCP proxying instead of HTTP Host-header
/// routing (see `ProxyTarget::listen_port`): no Host header exists to route
/// by, so `path_prefix`/`ssl` (HTTP-only concepts) cannot be combined with
/// it, ports 80/443 stay reserved for HTTP ingress, and each TCP route
/// needs a public port not already claimed by another service in this same
/// project (a *different* project sharing the same host can still collide
/// on `listen_port` -- that can't be caught here, since validation is
/// per-project only, and is instead rejected at apply time by jiji-proxy
/// itself, the one component with the whole host's picture).
fn validate_tcp_targets(config: &Config, errors: &mut Vec<ValidationError>) {
    let mut seen_listen_ports: std::collections::HashMap<u16, String> =
        std::collections::HashMap::new();
    for (name, service) in &config.services {
        let Some(proxy) = &service.proxy else {
            continue;
        };
        if let Some(targets) = &proxy.targets {
            for (index, target) in targets.iter().enumerate() {
                validate_tcp_target(
                    name,
                    &format!("services.{name}.proxy.targets[{index}]"),
                    target.listen_port,
                    target.path_prefix.as_deref(),
                    &target.ssl,
                    &mut seen_listen_ports,
                    errors,
                );
            }
        } else {
            validate_tcp_target(
                name,
                &format!("services.{name}.proxy"),
                proxy.listen_port,
                proxy.path_prefix.as_deref(),
                &proxy.ssl,
                &mut seen_listen_ports,
                errors,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_tcp_target(
    service_name: &str,
    path: &str,
    listen_port: Option<u16>,
    path_prefix: Option<&str>,
    ssl: &Option<SslValue>,
    seen_listen_ports: &mut std::collections::HashMap<u16, String>,
    errors: &mut Vec<ValidationError>,
) {
    let Some(listen_port) = listen_port else {
        return;
    };
    if path_prefix.is_some() || ssl.is_some() {
        errors.push(ValidationError {
            path: path.to_string(),
            message: format!(
                "Service '{service_name}' proxy target sets 'listen_port' (raw TCP mode) alongside 'path_prefix'/'ssl', which only apply to HTTP routing. Remove 'listen_port' for an HTTP route, or remove 'path_prefix'/'ssl' for a TCP route."
            ),
            code: "PROXY_TCP_HTTP_FIELDS_CONFLICT",
        });
    }
    if listen_port == 0 || listen_port == 80 || listen_port == 443 {
        errors.push(ValidationError {
            path: path.to_string(),
            message: format!(
                "Service '{service_name}' proxy target 'listen_port' {listen_port} is invalid: ports 80 and 443 are reserved for HTTP ingress, and 0 is not a valid port."
            ),
            code: "PROXY_INVALID_TCP_PORT",
        });
        return;
    }
    if let Some(existing_service) = seen_listen_ports.get(&listen_port) {
        errors.push(ValidationError {
            path: path.to_string(),
            message: format!(
                "Service '{service_name}' proxy target 'listen_port' {listen_port} is already used by service '{existing_service}'. Each TCP route needs its own public port within a project."
            ),
            code: "PROXY_TCP_PORT_CONFLICT",
        });
        return;
    }
    seen_listen_ports.insert(listen_port, service_name.to_string());
}

/// `network_mode: host` shares the host's own network stack, so `ports:` exists only as metadata
/// (what the service listens on), never as a `-p` mapping: at most one entry, a bare
/// container-side port number, never a `host:container` mapping or a `/udp` suffix. Two
/// `host`-mode services whose `servers:` sets overlap must not declare the same port, since both
/// containers would try to bind it on the same machine. Project-scoped only, the same
/// cross-project blind spot `validate_tcp_targets` already discloses for its own port space: a
/// different project's `host`-mode service on a shared machine can still collide, surfacing at
/// container start instead.
fn validate_host_network_ports(config: &Config, errors: &mut Vec<ValidationError>) {
    let mut seen_ports: Vec<(u16, &str, &Vec<String>)> = Vec::new();
    for (name, service) in &config.services {
        if service.network_mode != "host" {
            continue;
        }
        if service.ports.len() > 1 {
            errors.push(ValidationError {
                path: format!("services.{name}.ports"),
                message: format!(
                    "Service '{name}' uses network_mode: host and can declare at most one port (the port it listens on); found {}",
                    service.ports.len()
                ),
                code: "HOST_NETWORK_TOO_MANY_PORTS",
            });
            continue;
        }
        let Some(raw_port) = service.ports.first() else {
            continue;
        };
        let Ok(port) = raw_port.parse::<u16>() else {
            errors.push(ValidationError {
                path: format!("services.{name}.ports"),
                message: format!(
                    "Service '{name}' uses network_mode: host; 'ports' entry '{raw_port}' must be a bare container port number, not a host:container mapping or a /udp suffix -- the app must already bind the port it wants."
                ),
                code: "HOST_NETWORK_INVALID_PORT",
            });
            continue;
        };
        for (existing_port, existing_service, existing_servers) in &seen_ports {
            if *existing_port == port
                && service
                    .servers
                    .iter()
                    .any(|host| existing_servers.contains(host))
            {
                errors.push(ValidationError {
                    path: format!("services.{name}.ports"),
                    message: format!(
                        "Service '{name}' uses network_mode: host on port {port}, already used by service '{existing_service}' on a shared server. Each host-mode service needs its own port on any server it shares."
                    ),
                    code: "HOST_NETWORK_PORT_CONFLICT",
                });
            }
        }
        seen_ports.push((port, name, &service.servers));
    }
}

/// A cron container has no network namespace of its own to lease into once its service already
/// shares an upstream's (see `docs/architecture-notes.md#container-namespace-sharing`); this
/// stays unsupported for the first release rather than guessing at borrowed-namespace semantics.
fn validate_service_crons(
    service_name: &str,
    service: &Service,
    errors: &mut Vec<ValidationError>,
) {
    if service.crons.is_empty() {
        return;
    }
    if service.network_mode_dependency().is_some() {
        errors.push(ValidationError {
            path: format!("services.{service_name}.crons"),
            message: format!(
                "Service '{service_name}' cannot define 'crons' while using network_mode: service:<name>; a namespace-sharing dependent has no address of its own to lease for a cron container"
            ),
            code: "CRON_UNSUPPORTED_ON_NETWORK_MODE_SERVICE",
        });
    }
    if service.crons.len() > MAX_CRONS_PER_SERVICE {
        errors.push(ValidationError {
            path: format!("services.{service_name}.crons"),
            message: format!(
                "Service '{service_name}' supports at most {MAX_CRONS_PER_SERVICE} cron jobs; {} are configured",
                service.crons.len()
            ),
            code: "TOO_MANY_CRONS_PER_SERVICE",
        });
    }
    for (cron_name, cron) in &service.crons {
        let path = format!("services.{service_name}.crons.{cron_name}");
        if let Err(message) = validate_cron_name(cron_name) {
            errors.push(ValidationError {
                path: path.clone(),
                message: format!("Cron '{cron_name}' on service '{service_name}': {message}"),
                code: "CRON_NAME_INVALID",
            });
        }
        if let Err(message) = validate_cron_schedule(&cron.schedule) {
            errors.push(ValidationError {
                path: format!("{path}.schedule"),
                message: format!(
                    "Cron '{cron_name}' on service '{service_name}' has an invalid schedule '{}': {message}",
                    cron.schedule
                ),
                code: "CRON_SCHEDULE_INVALID",
            });
        }
        if let Err(message) = validate_cron_timezone(&cron.timezone) {
            errors.push(ValidationError {
                path: format!("{path}.timezone"),
                message: format!(
                    "Cron '{cron_name}' on service '{service_name}' has an invalid timezone '{}': {message}",
                    cron.timezone
                ),
                code: "CRON_TIMEZONE_INVALID",
            });
        }
        if command_is_empty(&cron.command) {
            errors.push(ValidationError {
                path: format!("{path}.command"),
                message: format!(
                    "Cron '{cron_name}' on service '{service_name}' must specify a non-empty command"
                ),
                code: "CRON_COMMAND_EMPTY",
            });
        }
        match cron.timeout_duration() {
            Some(duration) if !duration.is_zero() => {}
            _ => {
                errors.push(ValidationError {
                    path: format!("{path}.timeout"),
                    message: format!(
                        "Cron '{cron_name}' on service '{service_name}' has an invalid timeout '{}': expected a positive duration like '30s', '5m', or '1h'",
                        cron.timeout
                    ),
                    code: "CRON_TIMEOUT_INVALID",
                });
            }
        }
    }
}

/// Mirrors the DNS-safe server-name convention documented in `jiji.yml`: lowercase alphanumeric
/// and hyphens, no leading/trailing hyphen. A cron name becomes a container-name component
/// (`docs/architecture-notes.md#execution-model`'s
/// `{project}-{service}-cron-{cron_name}-{run_id}` form).
fn validate_cron_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("cron name must not be empty".to_string());
    }
    if name.len() > 63 {
        return Err(format!(
            "cron name is {} characters, longer than the 63-character limit",
            name.len()
        ));
    }
    let well_formed = name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-');
    if !well_formed {
        return Err(
            "cron name must be lowercase alphanumeric characters and hyphens, and cannot start or end with a hyphen"
                .to_string(),
        );
    }
    Ok(())
}

/// Requires exactly 5 whitespace-separated fields (standard cron: minute, hour, day-of-month,
/// month, day-of-week) before ever handing the expression to `jiff_cron`: this is what rejects a
/// seconds field or an alias like `@daily` outright, since neither splits into 5 fields. The
/// remaining field syntax (ranges, steps, lists, names) is validated by prepending a `0` seconds
/// field and delegating to `jiff_cron::Schedule`'s own parser rather than reimplementing it.
fn validate_cron_schedule(expression: &str) -> Result<(), String> {
    let field_count = expression.split_whitespace().count();
    if field_count != 5 {
        return Err(format!(
            "must have exactly 5 space-separated fields (minute hour day-of-month month day-of-week); found {field_count} field(s). Seconds fields and aliases like '@daily' are not supported"
        ));
    }
    jiff_cron::Schedule::from_str(&format!("0 {expression}"))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn validate_cron_timezone(name: &str) -> Result<(), String> {
    jiff::tz::TimeZone::get(name)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn command_is_empty(command: &CommandValue) -> bool {
    match command {
        CommandValue::Single(s) => s.trim().is_empty(),
        CommandValue::Multiple(parts) => {
            parts.is_empty() || parts.iter().all(|p| p.trim().is_empty())
        }
    }
}
