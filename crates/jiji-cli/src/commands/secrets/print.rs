use std::collections::BTreeSet;

use jiji_config::{BuildValue, CommandValue, Config, SslValue};
use jiji_tui::Ui;

use crate::env_resolution::{self, is_bare_all_caps_name};

pub fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    services: Option<&str>,
    host_env: bool,
    show_values: bool,
) -> anyhow::Result<()> {
    Ui::section("Secrets Configuration:");

    let start = std::env::current_dir()?;
    let config_path = config_file.map(std::path::Path::new);
    let (config, path) = jiji_config::load_config(environment, config_path, &start)?;

    let validation = jiji_config::validate_config(&config);
    if !validation.valid {
        for error in validation.errors {
            Ui::error(&format!("{}: {}", error.path, error.message));
        }
        anyhow::bail!("Configuration is invalid; fix the errors above and try again");
    }

    let project_root = env_resolution::project_root_from_config_path(&path);
    let (loaded_env, loaded_from) =
        env_resolution::load_env_file(&project_root, environment, config.secrets_path.as_deref())?;

    Ui::say(
        &format!("Environment: {}", environment.unwrap_or("default")),
        1,
    );
    Ui::say(&format!("Config file: {}", path.display()), 1);
    Ui::say(&format!("Project root: {}", project_root.display()), 1);
    match &loaded_from {
        Some(loaded_from) => Ui::say(&format!("Secrets file: {}", loaded_from.display()), 1),
        None => Ui::say("Secrets file: not found", 1),
    }
    Ui::say(
        &format!(
            "Host env fallback: {}",
            if host_env { "enabled" } else { "disabled" }
        ),
        1,
    );

    let service_filter = split_comma_trimmed(services);
    let refs = collect_secret_refs(&config, &service_filter);

    if refs.is_empty() {
        Ui::say("No secret references found in configuration.", 0);
        return Ok(());
    }

    let mut informational_only = false;
    for (source, group) in group_by_source(&refs) {
        Ui::section(&format!("{source}:"));
        for secret_ref in group {
            informational_only |= !secret_ref.runtime_resolved;
            let resolved =
                env_resolution::resolve_secret_name(&secret_ref.name, &loaded_env, host_env);
            let status = match (&resolved, show_values) {
                (Some(value), true) => value.clone(),
                (Some(_), false) => "[SET]".to_string(),
                (None, _) => "[MISSING]".to_string(),
            };
            Ui::say(&format!("{}: {}", secret_ref.name, status), 1);
        }
    }

    let unique_names: BTreeSet<&str> = refs.iter().map(|r| r.name.as_str()).collect();
    let missing_count = unique_names
        .iter()
        .filter(|name| env_resolution::resolve_secret_name(name, &loaded_env, host_env).is_none())
        .count();

    println!();
    if missing_count > 0 {
        Ui::warn(&format!(
            "{missing_count} secret(s) are missing. Deployment will fail until these are provided."
        ));
        if !host_env {
            Ui::say(
                "Tip: pass --host-env to also check host environment variables",
                1,
            );
        }
    } else {
        Ui::success("All secrets are configured correctly.");
    }

    if informational_only {
        println!();
        Ui::say(
            "Note: ssh key_passphrase/key_data, proxy SSL certs, build args, and command \
             interpolation are reported for visibility only -- unlike environment.secrets and \
             builder.registry.password, they are used as literal config values today and are not \
             actually substituted from .env/host env at runtime.",
            0,
        );
    }

    Ok(())
}

fn split_comma_trimmed(value: Option<&str>) -> Vec<String> {
    value
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn service_matches(name: &str, filters: &[String]) -> bool {
    filters.is_empty()
        || filters
            .iter()
            .any(|filter| jiji_core::matches_pattern(name, filter))
}

/// A secret-shaped reference found in the configuration, with context about where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SecretRef {
    name: String,
    source: String,
    /// Whether this config field is actually resolved from `.env`/host-env by any runtime code
    /// path today (`environment.secrets`, `builder.registry.password`), versus reported here only
    /// for visibility because it looks like a secret reference but is currently used literally
    /// (ssh key_passphrase/key_data, proxy SSL certs, build args, command interpolation).
    runtime_resolved: bool,
}

fn push_if_env_ref(
    refs: &mut Vec<SecretRef>,
    value: Option<&str>,
    source: &str,
    runtime_resolved: bool,
) {
    if let Some(value) = value {
        if is_bare_all_caps_name(value) {
            refs.push(SecretRef {
                name: value.to_string(),
                source: source.to_string(),
                runtime_resolved,
            });
        }
    }
}

fn push_ssl_ref(refs: &mut Vec<SecretRef>, ssl: Option<&SslValue>, source: &str) {
    if let Some(SslValue::Certs {
        certificate_pem,
        private_key_pem,
    }) = ssl
    {
        push_if_env_ref(refs, Some(certificate_pem), source, false);
        push_if_env_ref(refs, Some(private_key_pem), source, false);
    }
}

/// `${VAR}` references inside a rendered command string, `VAR` restricted to the same bare
/// ALL_CAPS name shape as every other secret reference in this scan.
fn command_var_refs(command: &str) -> Vec<String> {
    let mut refs = Vec::new();
    let mut remaining = command;
    while let Some(start) = remaining.find("${") {
        let after_open = &remaining[start + 2..];
        let Some(end) = after_open.find('}') else {
            break;
        };
        let name = &after_open[..end];
        if is_bare_all_caps_name(name) {
            refs.push(name.to_string());
        }
        remaining = &after_open[end + 1..];
    }
    refs
}

/// Scans the entire configuration for secret-shaped references (see the module doc on
/// `SecretRef::runtime_resolved` for which of these are actually resolved from `.env`/host-env
/// today versus reported for visibility only), matching the POC's `secrets print` coverage.
fn collect_secret_refs(config: &Config, service_filter: &[String]) -> Vec<SecretRef> {
    let mut refs = Vec::new();

    let mut server_names: Vec<&String> = config.servers.keys().collect();
    server_names.sort();
    for name in server_names {
        let server = &config.servers[name];
        let source = format!("servers.{name}.host");
        push_if_env_ref(&mut refs, Some(server.host.as_str()), &source, false);

        let source = format!("servers.{name}.ssh");
        push_if_env_ref(&mut refs, server.key_passphrase.as_deref(), &source, false);
        for entry in server.key_data.iter().flatten() {
            push_if_env_ref(&mut refs, Some(entry.as_str()), &source, false);
        }
    }

    if let Some(ssh) = &config.ssh {
        push_if_env_ref(&mut refs, ssh.key_passphrase.as_deref(), "ssh", false);
        for entry in ssh.key_data.iter().flatten() {
            push_if_env_ref(&mut refs, Some(entry.as_str()), "ssh.key_data", false);
        }
    }

    push_if_env_ref(
        &mut refs,
        config.builder.registry.password.as_deref(),
        "builder.registry",
        true,
    );

    if let Some(environment) = &config.environment {
        for secret in &environment.secrets {
            refs.push(SecretRef {
                name: secret.clone(),
                source: "environment.secrets".to_string(),
                runtime_resolved: true,
            });
        }
    }

    let mut service_names: Vec<&String> = config
        .services
        .keys()
        .filter(|name| service_matches(name, service_filter))
        .collect();
    service_names.sort();

    for name in service_names {
        let service = &config.services[name];
        let prefix = format!("services.{name}");

        for secret in &service.environment.secrets {
            refs.push(SecretRef {
                name: secret.clone(),
                source: format!("{prefix}.environment.secrets"),
                runtime_resolved: true,
            });
        }

        if let Some(proxy) = &service.proxy {
            let source = format!("{prefix}.proxy.ssl");
            push_ssl_ref(&mut refs, proxy.ssl.as_ref(), &source);
            for target in proxy.targets.iter().flatten() {
                push_ssl_ref(&mut refs, target.ssl.as_ref(), &source);
            }
        }

        if let Some(BuildValue::Detailed(build)) = &service.build {
            for (key, value) in build.args.iter().flatten() {
                push_if_env_ref(
                    &mut refs,
                    Some(value.as_str()),
                    &format!("{prefix}.build.args.{key}"),
                    false,
                );
            }
        }

        if let Some(command) = &service.command {
            let parts: Vec<&str> = match command {
                CommandValue::Single(value) => vec![value.as_str()],
                CommandValue::Multiple(values) => values.iter().map(String::as_str).collect(),
            };
            let source = format!("{prefix}.command");
            for part in parts {
                for name in command_var_refs(part) {
                    refs.push(SecretRef {
                        name,
                        source: source.clone(),
                        runtime_resolved: false,
                    });
                }
            }
        }
    }

    refs
}

fn group_by_source(refs: &[SecretRef]) -> Vec<(&str, Vec<&SecretRef>)> {
    let mut groups: Vec<(&str, Vec<&SecretRef>)> = Vec::new();
    for secret_ref in refs {
        match groups
            .iter_mut()
            .find(|(source, _)| *source == secret_ref.source)
        {
            Some((_, group)) => group.push(secret_ref),
            None => groups.push((secret_ref.source.as_str(), vec![secret_ref])),
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiji_config::{
        BuildConfig, Builder, ContainerEngine, Environment, NamedServer, Service, Ssh,
    };
    use std::collections::HashMap;

    fn base_config() -> Config {
        Config {
            project: "demo".to_string(),
            builder: Builder {
                engine: ContainerEngine::Docker,
                local: true,
                remote: None,
                cache: true,
                registry: Default::default(),
            },
            servers: HashMap::new(),
            services: HashMap::new(),
            ssh: None,
            network: None,
            secrets_path: None,
            secrets: None,
            environment: None,
        }
    }

    #[test]
    fn collects_project_level_environment_secrets() {
        let mut config = base_config();
        config.environment = Some(Environment {
            clear: HashMap::new(),
            secrets: vec!["DATABASE_URL".to_string()],
        });
        let refs = collect_secret_refs(&config, &[]);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "DATABASE_URL");
        assert_eq!(refs[0].source, "environment.secrets");
        assert!(refs[0].runtime_resolved);
    }

    #[test]
    fn collects_per_service_environment_secrets_and_respects_service_filter() {
        let mut config = base_config();
        config.services.insert(
            "web".to_string(),
            Service {
                environment: Environment {
                    clear: HashMap::new(),
                    secrets: vec!["API_KEY".to_string()],
                },
                ..default_service()
            },
        );
        config.services.insert(
            "worker".to_string(),
            Service {
                environment: Environment {
                    clear: HashMap::new(),
                    secrets: vec!["QUEUE_TOKEN".to_string()],
                },
                ..default_service()
            },
        );

        let all = collect_secret_refs(&config, &[]);
        assert_eq!(all.len(), 2);

        let filtered = collect_secret_refs(&config, &["web".to_string()]);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "API_KEY");
    }

    #[test]
    fn registry_password_is_only_a_ref_when_it_looks_like_an_env_var_name() {
        let mut config = base_config();
        config.builder.registry.password = Some("literal-password".to_string());
        assert!(collect_secret_refs(&config, &[]).is_empty());

        config.builder.registry.password = Some("REGISTRY_PASSWORD".to_string());
        let refs = collect_secret_refs(&config, &[]);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "REGISTRY_PASSWORD");
        assert!(refs[0].runtime_resolved);
    }

    #[test]
    fn ssh_key_passphrase_and_key_data_are_flagged_as_informational_only() {
        let mut config = base_config();
        config.ssh = Some(Ssh {
            key_passphrase: Some("SSH_PASSPHRASE".to_string()),
            key_data: Some(vec!["SSH_KEY_DATA".to_string()]),
            ..default_ssh()
        });
        let refs = collect_secret_refs(&config, &[]);
        assert_eq!(refs.len(), 2);
        assert!(refs.iter().all(|r| !r.runtime_resolved));
    }

    #[test]
    fn server_host_and_ssh_overrides_are_scanned() {
        let mut config = base_config();
        config.servers.insert(
            "app1".to_string(),
            NamedServer {
                host: "APP1_HOST".to_string(),
                arch: None,
                user: None,
                port: None,
                key_path: None,
                key_passphrase: Some("APP1_PASSPHRASE".to_string()),
                keys: None,
                key_data: None,
            },
        );
        let refs = collect_secret_refs(&config, &[]);
        assert_eq!(refs.len(), 2);
        assert!(refs
            .iter()
            .any(|r| r.name == "APP1_HOST" && r.source == "servers.app1.host"));
        assert!(refs
            .iter()
            .any(|r| r.name == "APP1_PASSPHRASE" && r.source == "servers.app1.ssh"));
    }

    #[test]
    fn proxy_ssl_certs_are_scanned_on_single_and_multi_target_configs() {
        use jiji_config::{ProxyConfig, ProxyTarget};

        let mut config = base_config();
        config.services.insert(
            "web".to_string(),
            Service {
                proxy: Some(ProxyConfig {
                    app_port: Some(3000),
                    host: None,
                    hosts: None,
                    ssl: Some(SslValue::Certs {
                        certificate_pem: "CERT_PEM".to_string(),
                        private_key_pem: "KEY_PEM".to_string(),
                    }),
                    path_prefix: None,
                    healthcheck: None,
                    targets: Some(vec![ProxyTarget {
                        app_port: 4000,
                        host: None,
                        hosts: None,
                        ssl: Some(SslValue::Certs {
                            certificate_pem: "CERT_PEM_2".to_string(),
                            private_key_pem: "KEY_PEM_2".to_string(),
                        }),
                        path_prefix: None,
                        healthcheck: None,
                    }]),
                }),
                ..default_service()
            },
        );
        let refs = collect_secret_refs(&config, &[]);
        let names: Vec<&str> = refs.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"CERT_PEM"));
        assert!(names.contains(&"KEY_PEM"));
        assert!(names.contains(&"CERT_PEM_2"));
        assert!(names.contains(&"KEY_PEM_2"));
        assert!(refs.iter().all(|r| !r.runtime_resolved));
    }

    #[test]
    fn build_args_are_scanned_for_env_var_shaped_values() {
        let mut config = base_config();
        let mut args = HashMap::new();
        args.insert("DB_PASSWORD".to_string(), "DB_PASSWORD_SECRET".to_string());
        args.insert("MODE".to_string(), "production".to_string());
        config.services.insert(
            "web".to_string(),
            Service {
                build: Some(BuildValue::Detailed(BuildConfig {
                    context: ".".to_string(),
                    dockerfile: None,
                    args: Some(args),
                    target: None,
                })),
                ..default_service()
            },
        );
        let refs = collect_secret_refs(&config, &[]);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "DB_PASSWORD_SECRET");
        assert_eq!(refs[0].source, "services.web.build.args.DB_PASSWORD");
    }

    #[test]
    fn command_interpolation_refs_are_extracted() {
        let mut config = base_config();
        config.services.insert(
            "web".to_string(),
            Service {
                command: Some(CommandValue::Single(
                    "run --token=${API_TOKEN} --mode=live".to_string(),
                )),
                ..default_service()
            },
        );
        let refs = collect_secret_refs(&config, &[]);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "API_TOKEN");
        assert_eq!(refs[0].source, "services.web.command");
    }

    #[test]
    fn command_var_refs_ignores_lowercase_and_unclosed_braces() {
        assert_eq!(command_var_refs("${lower_case}"), Vec::<String>::new());
        assert_eq!(command_var_refs("${UNCLOSED"), Vec::<String>::new());
        assert_eq!(command_var_refs("${A} and ${B}"), vec!["A", "B"]);
    }

    #[test]
    fn grouping_preserves_first_seen_source_order() {
        let refs = vec![
            SecretRef {
                name: "A".to_string(),
                source: "ssh".to_string(),
                runtime_resolved: false,
            },
            SecretRef {
                name: "B".to_string(),
                source: "environment.secrets".to_string(),
                runtime_resolved: true,
            },
            SecretRef {
                name: "C".to_string(),
                source: "ssh".to_string(),
                runtime_resolved: false,
            },
        ];
        let grouped = group_by_source(&refs);
        assert_eq!(grouped[0].0, "ssh");
        assert_eq!(grouped[0].1.len(), 2);
        assert_eq!(grouped[1].0, "environment.secrets");
    }

    fn default_service() -> Service {
        Service {
            image: None,
            build: None,
            servers: Vec::new(),
            ports: Vec::new(),
            volumes: Vec::new(),
            files: Vec::new(),
            directories: Vec::new(),
            environment: Environment {
                clear: HashMap::new(),
                secrets: Vec::new(),
            },
            command: None,
            proxy: None,
            retain: 0,
            network_mode: "bridge".to_string(),
            cpus: None,
            memory: None,
            gpus: None,
            devices: Vec::new(),
            privileged: false,
            cap_add: Vec::new(),
            stop_first: false,
            restart: None,
        }
    }

    fn default_ssh() -> Ssh {
        Ssh {
            user: None,
            port: 22,
            key_path: None,
            key_passphrase: None,
            connect_timeout: 10,
            command_timeout: 60,
            options: HashMap::new(),
            proxy: None,
            proxy_command: None,
            keys: None,
            key_data: None,
            keys_only: false,
            max_concurrent_starts: 4,
            pool_idle_timeout: 60,
            dns_retries: 3,
            log_level: Default::default(),
            config: Default::default(),
        }
    }
}
