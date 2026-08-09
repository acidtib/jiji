use jiji_config::{validate_yaml, Config, TEMPLATE};

fn parse(yaml: &str) -> serde_yaml::Value {
    serde_yaml::from_str(yaml).expect("test fixture must be valid YAML")
}

#[test]
fn missing_top_level_field_reports_missing_field() {
    let raw = parse(
        r#"
project: demo
builder:
  engine: podman
servers: {}
"#,
    );
    let result = validate_yaml(&raw);
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == "MISSING_FIELD" && e.path == "services"));
}

#[test]
fn service_with_no_servers_reports_no_servers() {
    let raw = parse(
        r#"
project: demo
builder:
  engine: podman
servers:
  web:
    host: 10.0.0.1
services:
  app:
    image: nginx:latest
"#,
    );
    let result = validate_yaml(&raw);
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == "NO_SERVERS" && e.path == "services.app.servers"));
}

#[test]
fn service_server_referencing_undefined_server_reports_undefined_server() {
    let raw = parse(
        r#"
project: demo
builder:
  engine: podman
servers:
  web:
    host: 10.0.0.1
services:
  app:
    image: nginx:latest
    servers:
      - not-a-real-server
"#,
    );
    let result = validate_yaml(&raw);
    assert!(!result.valid);
    let err = result
        .errors
        .iter()
        .find(|e| e.code == "UNDEFINED_SERVER")
        .expect("expected an UNDEFINED_SERVER error");
    assert!(err.message.contains("not-a-real-server"));
    assert!(err.message.contains("web"));
}

#[test]
fn replicas_and_placement_have_distributed_defaults() {
    let config: jiji_config::Config = serde_yaml::from_str(
        r#"
project: demo
builder: { engine: podman }
servers:
  one: { host: 10.0.0.1 }
services:
  web:
    image: nginx
    servers: [one]
"#,
    )
    .unwrap();
    assert_eq!(config.services["web"].replicas, 1);
    assert_eq!(
        config.services["web"].placement,
        jiji_config::PlacementPolicy::Spread
    );
}

#[test]
fn stop_first_and_local_state_cannot_be_scaled() {
    let raw = parse(
        r#"
project: demo
builder: { engine: podman }
servers:
  one: { host: 10.0.0.1 }
services:
  database:
    image: postgres
    servers: [one]
    replicas: 2
    stop_first: true
    volumes: [data:/var/lib/postgresql/data]
"#,
    );
    let result = validate_yaml(&raw);
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|error| error.code == "STOP_FIRST_REQUIRES_SINGLETON"));
    assert!(result
        .errors
        .iter()
        .any(|error| error.code == "STATEFUL_SCALE"));
}

#[test]
fn container_namespace_networking_is_rejected() {
    let raw = parse(
        r#"
project: demo
builder: { engine: podman }
servers:
  one: { host: 10.0.0.1 }
services:
  sidecar:
    image: busybox
    servers: [one]
    network_mode: container:other
"#,
    );
    let result = validate_yaml(&raw);
    assert!(result
        .errors
        .iter()
        .any(|error| error.code == "UNSUPPORTED_NETWORK_MODE"));
}

#[test]
fn network_mode_service_referencing_undefined_service_is_rejected() {
    let raw = parse(
        r#"
project: demo
builder: { engine: podman }
servers:
  one: { host: 10.0.0.1 }
services:
  qbittorrent:
    image: qbittorrent
    servers: [one]
    network_mode: service:gluetun
"#,
    );
    let result = validate_yaml(&raw);
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|error| error.code == "UNDEFINED_NETWORK_MODE_SERVICE"));
}

#[test]
fn network_mode_service_self_reference_is_rejected() {
    let raw = parse(
        r#"
project: demo
builder: { engine: podman }
servers:
  one: { host: 10.0.0.1 }
services:
  gluetun:
    image: gluetun
    servers: [one]
    network_mode: service:gluetun
"#,
    );
    let result = validate_yaml(&raw);
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|error| error.code == "NETWORK_MODE_SERVICE_SELF_REFERENCE"));
}

#[test]
fn network_mode_service_chain_is_rejected() {
    let raw = parse(
        r#"
project: demo
builder: { engine: podman }
servers:
  one: { host: 10.0.0.1 }
services:
  gluetun:
    image: gluetun
    servers: [one]
  qbittorrent:
    image: qbittorrent
    servers: [one]
    network_mode: service:gluetun
  sonarr-sidecar:
    image: sonarr
    servers: [one]
    network_mode: service:qbittorrent
"#,
    );
    let result = validate_yaml(&raw);
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|error| error.code == "NETWORK_MODE_SERVICE_CHAIN_UNSUPPORTED"));
}

#[test]
fn network_mode_service_server_mismatch_is_rejected() {
    let raw = parse(
        r#"
project: demo
builder: { engine: podman }
servers:
  one: { host: 10.0.0.1 }
  two: { host: 10.0.0.2 }
services:
  gluetun:
    image: gluetun
    servers: [one]
  qbittorrent:
    image: qbittorrent
    servers: [two]
    network_mode: service:gluetun
"#,
    );
    let result = validate_yaml(&raw);
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|error| error.code == "NETWORK_MODE_SERVICE_SERVER_MISMATCH"));
}

#[test]
fn network_mode_service_dependent_still_rejects_scale_and_proxy() {
    let raw = parse(
        r#"
project: demo
builder: { engine: podman }
servers:
  one: { host: 10.0.0.1 }
services:
  gluetun:
    image: gluetun
    servers: [one]
  qbittorrent:
    image: qbittorrent
    servers: [one]
    network_mode: service:gluetun
    replicas: 2
    proxy:
      port: 8080
"#,
    );
    let result = validate_yaml(&raw);
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|error| error.code == "NON_BRIDGE_SCALE"));
    assert!(result
        .errors
        .iter()
        .any(|error| error.code == "NON_BRIDGE_PROXY"));
}

#[test]
fn network_mode_service_valid_dependency_validates_cleanly() {
    let raw = parse(
        r#"
project: demo
builder: { engine: podman }
servers:
  one: { host: 10.0.0.1 }
services:
  gluetun:
    image: gluetun
    servers: [one]
  qbittorrent:
    image: qbittorrent
    servers: [one]
    network_mode: service:gluetun
"#,
    );
    let result = validate_yaml(&raw);
    assert!(!result
        .errors
        .iter()
        .any(|error| error.code.starts_with("NETWORK_MODE_SERVICE")
            || error.code == "UNDEFINED_NETWORK_MODE_SERVICE"));
}

#[test]
fn ssh_section_with_zero_port_reports_invalid_port() {
    let raw = parse(
        r#"
project: demo
builder:
  engine: podman
servers:
  web:
    host: 10.0.0.1
services:
  app:
    image: nginx:latest
    servers:
      - web
ssh:
  user: root
  port: 0
"#,
    );
    let result = validate_yaml(&raw);
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == "INVALID_PORT" && e.path == "ssh.port"));
}

#[test]
fn ssh_section_missing_user_reports_missing_field() {
    let raw = parse(
        r#"
project: demo
builder:
  engine: podman
servers:
  web:
    host: 10.0.0.1
services:
  app:
    image: nginx:latest
    servers:
      - web
ssh:
  port: 22
"#,
    );
    let result = validate_yaml(&raw);
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == "MISSING_FIELD" && e.path == "ssh.user"));
}

#[test]
fn ssh_user_may_come_from_enabled_ssh_config() {
    let raw = parse(
        r#"
project: demo
builder:
  engine: podman
servers:
  web:
    host: production
services:
  app:
    image: nginx:latest
    servers:
      - web
ssh:
  config: true
"#,
    );
    let result = validate_yaml(&raw);
    assert!(result.valid, "unexpected errors: {:?}", result.errors);
}

#[test]
fn every_server_may_define_its_own_ssh_user() {
    let raw = parse(
        r#"
project: demo
builder:
  engine: podman
servers:
  web:
    host: 10.0.0.1
    user: deploy
services:
  app:
    image: nginx:latest
    servers:
      - web
ssh:
  port: 22
"#,
    );
    let result = validate_yaml(&raw);
    assert!(result.valid, "unexpected errors: {:?}", result.errors);
}

#[test]
fn shipped_template_parses_and_validates_cleanly() {
    let raw = parse(TEMPLATE);
    let result = validate_yaml(&raw);
    assert!(
        result.valid,
        "shipped jiji.yml template should validate cleanly, got errors: {:?}",
        result.errors
    );
}

#[test]
fn builder_remote_selects_remote_even_with_legacy_local_true() {
    let raw = parse(
        r#"
project: demo
builder:
  engine: podman
  local: true
  remote: ssh://build@10.0.0.9
servers:
  web:
    host: 10.0.0.1
services:
  app:
    image: nginx:latest
    servers: [web]
"#,
    );
    let result = validate_yaml(&raw);
    assert!(result.valid, "unexpected errors: {:?}", result.errors);
}

#[test]
fn legacy_builder_local_false_without_remote_still_defaults_to_local() {
    let raw = parse(
        r#"
project: demo
builder:
  engine: podman
  local: false
servers:
  web:
    host: 10.0.0.1
services:
  app:
    image: nginx:latest
    servers: [web]
"#,
    );
    let result = validate_yaml(&raw);
    assert!(result.valid, "unexpected errors: {:?}", result.errors);
}

#[test]
fn builder_remote_with_invalid_uri_reports_invalid_builder_remote() {
    let raw = parse(
        r#"
project: demo
builder:
  engine: podman
  local: false
  remote: ssh://user:pass@10.0.0.9
servers:
  web:
    host: 10.0.0.1
services:
  app:
    image: nginx:latest
    servers: [web]
"#,
    );
    let result = validate_yaml(&raw);
    assert!(!result.valid);
    let err = result
        .errors
        .iter()
        .find(|e| e.code == "INVALID_BUILDER_REMOTE")
        .expect("expected an INVALID_BUILDER_REMOTE error");
    assert_eq!(err.path, "builder.remote");
    assert!(err.message.contains("password"));
}

#[test]
fn builder_local_false_with_valid_remote_validates_cleanly() {
    let raw = parse(
        r#"
project: demo
builder:
  engine: podman
  local: false
  remote: ssh://build@10.0.0.9:2222
servers:
  web:
    host: 10.0.0.1
services:
  app:
    image: nginx:latest
    servers: [web]
"#,
    );
    let result = validate_yaml(&raw);
    assert!(result.valid, "unexpected errors: {:?}", result.errors);
}

#[test]
fn split_network_cidrs_parse_and_validate() {
    let raw = parse(
        r#"
project: demo
builder:
  engine: docker
servers:
  web:
    host: 203.0.113.10
services:
  app:
    image: nginx:latest
    servers: [web]
network:
  management_cidr: 198.18.0.0/16
  container_cidr: 100.64.0.0/10
"#,
    );
    let result = validate_yaml(&raw);
    assert!(result.valid, "unexpected errors: {:?}", result.errors);
}

#[test]
fn removed_legacy_key_fields_are_rejected_globally_and_per_server() {
    for (raw, removed_field) in [
        (
            r#"
project: demo
builder: { engine: podman }
servers:
  web: { host: 10.0.0.1 }
services:
  app: { image: nginx, servers: [web] }
ssh:
  user: root
  key_path: ~/.ssh/id_ed25519
"#,
            "key_path",
        ),
        (
            r#"
project: demo
builder: { engine: podman }
servers:
  web:
    host: 10.0.0.1
    key_path: ~/.ssh/id_ed25519
services:
  app: { image: nginx, servers: [web] }
ssh: { user: root }
"#,
            "key_path",
        ),
        (
            r#"
project: demo
builder: { engine: podman }
servers:
  web: { host: 10.0.0.1 }
services:
  app: { image: nginx, servers: [web] }
ssh:
  user: root
  key: ~/.ssh/id_ed25519
"#,
            "key",
        ),
        (
            r#"
project: demo
builder: { engine: podman }
servers:
  web:
    host: 10.0.0.1
    key: ~/.ssh/id_ed25519
services:
  app: { image: nginx, servers: [web] }
ssh: { user: root }
"#,
            "key",
        ),
    ] {
        let result = validate_yaml(&parse(raw));
        assert!(!result.valid);
        assert!(
            result
                .errors
                .iter()
                .any(|error| error.message.contains(removed_field)),
            "unexpected errors: {:?}",
            result.errors
        );
    }
}

#[test]
fn singular_proxy_host_is_rejected_for_flat_and_multi_target_configs() {
    for raw in [
        r#"
project: demo
builder: { engine: podman }
servers:
  web: { host: 10.0.0.1 }
services:
  app:
    image: nginx
    servers: [web]
    proxy: { port: 80, host: example.com }
"#,
        r#"
project: demo
builder: { engine: podman }
servers:
  web: { host: 10.0.0.1 }
services:
  app:
    image: nginx
    servers: [web]
    proxy:
      targets:
        - { port: 80, host: example.com }
"#,
    ] {
        let result = validate_yaml(&parse(raw));
        assert!(!result.valid);
        assert!(
            result
                .errors
                .iter()
                .any(|error| error.message.contains("host")),
            "unexpected errors: {:?}",
            result.errors
        );
    }
}

#[test]
fn removed_proxy_app_port_is_rejected_for_flat_and_multi_target_configs() {
    for raw in [
        r#"
project: demo
builder: { engine: podman }
servers:
  web: { host: 10.0.0.1 }
services:
  app:
    image: nginx
    servers: [web]
    proxy: { app_port: 80, hosts: [example.com] }
"#,
        r#"
project: demo
builder: { engine: podman }
servers:
  web: { host: 10.0.0.1 }
services:
  app:
    image: nginx
    servers: [web]
    proxy:
      targets:
        - { app_port: 80, hosts: [example.com] }
"#,
    ] {
        let result = validate_yaml(&parse(raw));
        assert!(!result.valid);
        assert!(
            result
                .errors
                .iter()
                .any(|error| error.message.contains("app_port")),
            "unexpected errors: {:?}",
            result.errors
        );
    }
}

#[test]
fn malformed_wildcard_proxy_hosts_are_rejected() {
    for bad_host in ["foo.*.com", "*foo.com", "*", "*.", "*.*.com"] {
        let raw = parse(&format!(
            r#"
project: demo
builder: {{ engine: podman }}
servers:
  web: {{ host: 10.0.0.1 }}
services:
  app:
    image: nginx
    servers: [web]
    proxy: {{ port: 80, hosts: ["{bad_host}"] }}
"#
        ));
        let result = validate_yaml(&raw);
        assert!(!result.valid, "expected '{bad_host}' to be rejected");
        assert!(
            result
                .errors
                .iter()
                .any(|error| error.code == "PROXY_INVALID_WILDCARD_HOST"),
            "unexpected errors for '{bad_host}': {:?}",
            result.errors
        );
    }
}

#[test]
fn well_formed_wildcard_proxy_host_with_ssl_true_is_rejected() {
    let raw = parse(
        r#"
project: demo
builder: { engine: podman }
servers:
  web: { host: 10.0.0.1 }
services:
  app:
    image: nginx
    servers: [web]
    proxy: { port: 80, hosts: ["*.example.com"], ssl: true }
"#,
    );
    let result = validate_yaml(&raw);
    assert!(!result.valid);
    assert!(
        result
            .errors
            .iter()
            .any(|error| error.code == "PROXY_WILDCARD_REQUIRES_STATIC_CERT"),
        "unexpected errors: {:?}",
        result.errors
    );
}

#[test]
fn well_formed_wildcard_proxy_host_with_static_cert_is_accepted() {
    let raw = parse(
        r#"
project: demo
builder: { engine: podman }
servers:
  web: { host: 10.0.0.1 }
services:
  app:
    image: nginx
    servers: [web]
    proxy:
      port: 80
      hosts: ["*.example.com"]
      ssl:
        certificate_pem: CERT
        private_key_pem: KEY
"#,
    );
    let result = validate_yaml(&raw);
    assert!(result.valid, "unexpected errors: {:?}", result.errors);
}

#[test]
fn well_formed_wildcard_proxy_host_without_ssl_is_accepted() {
    let raw = parse(
        r#"
project: demo
builder: { engine: podman }
servers:
  web: { host: 10.0.0.1 }
services:
  app:
    image: nginx
    servers: [web]
    proxy: { port: 80, hosts: ["*.example.com"] }
"#,
    );
    let result = validate_yaml(&raw);
    assert!(result.valid, "unexpected errors: {:?}", result.errors);
}

#[test]
fn wildcard_validation_also_applies_through_multi_target_proxy_configs() {
    let raw = parse(
        r#"
project: demo
builder: { engine: podman }
servers:
  web: { host: 10.0.0.1 }
services:
  app:
    image: nginx
    servers: [web]
    proxy:
      targets:
        - { port: 80, hosts: ["*.example.com"], ssl: true }
"#,
    );
    let result = validate_yaml(&raw);
    assert!(!result.valid);
    assert!(
        result
            .errors
            .iter()
            .any(|error| error.code == "PROXY_WILDCARD_REQUIRES_STATIC_CERT"),
        "unexpected errors: {:?}",
        result.errors
    );
}

#[test]
fn non_wildcard_proxy_hosts_are_unaffected() {
    let raw = parse(
        r#"
project: demo
builder: { engine: podman }
servers:
  web: { host: 10.0.0.1 }
services:
  app:
    image: nginx
    servers: [web]
    proxy: { port: 80, hosts: [example.com, www.example.com], ssl: true }
"#,
    );
    let result = validate_yaml(&raw);
    assert!(result.valid, "unexpected errors: {:?}", result.errors);
}

#[test]
fn tcp_listen_port_with_path_prefix_is_rejected() {
    let raw = parse(
        r#"
project: demo
builder: { engine: podman }
servers:
  web: { host: 10.0.0.1 }
services:
  db:
    image: postgres:18
    servers: [web]
    proxy: { port: 5432, listen_port: 5432, path_prefix: /admin }
"#,
    );
    let result = validate_yaml(&raw);
    assert!(!result.valid);
    assert!(
        result
            .errors
            .iter()
            .any(|error| error.code == "PROXY_TCP_HTTP_FIELDS_CONFLICT"),
        "unexpected errors: {:?}",
        result.errors
    );
}

#[test]
fn tcp_listen_port_with_ssl_is_rejected() {
    let raw = parse(
        r#"
project: demo
builder: { engine: podman }
servers:
  web: { host: 10.0.0.1 }
services:
  db:
    image: postgres:18
    servers: [web]
    proxy: { port: 5432, listen_port: 5432, ssl: false }
"#,
    );
    let result = validate_yaml(&raw);
    assert!(!result.valid);
    assert!(
        result
            .errors
            .iter()
            .any(|error| error.code == "PROXY_TCP_HTTP_FIELDS_CONFLICT"),
        "unexpected errors: {:?}",
        result.errors
    );
}

#[test]
fn tcp_listen_port_reserved_for_http_is_rejected() {
    for reserved in [80u16, 443, 0] {
        let raw = parse(&format!(
            r#"
project: demo
builder: {{ engine: podman }}
servers:
  web: {{ host: 10.0.0.1 }}
services:
  db:
    image: postgres:18
    servers: [web]
    proxy: {{ port: 5432, listen_port: {reserved} }}
"#
        ));
        let result = validate_yaml(&raw);
        assert!(!result.valid, "expected {reserved} to be rejected");
        assert!(
            result
                .errors
                .iter()
                .any(|error| error.code == "PROXY_INVALID_TCP_PORT"),
            "unexpected errors for {reserved}: {:?}",
            result.errors
        );
    }
}

#[test]
fn duplicate_tcp_listen_ports_in_the_same_project_are_rejected() {
    let raw = parse(
        r#"
project: demo
builder: { engine: podman }
servers:
  web: { host: 10.0.0.1 }
services:
  db:
    image: postgres:18
    servers: [web]
    proxy: { port: 5432, listen_port: 5432 }
  cache:
    image: redis
    servers: [web]
    proxy: { port: 6379, listen_port: 5432 }
"#,
    );
    let result = validate_yaml(&raw);
    assert!(!result.valid);
    assert!(
        result
            .errors
            .iter()
            .any(|error| error.code == "PROXY_TCP_PORT_CONFLICT"),
        "unexpected errors: {:?}",
        result.errors
    );
}

#[test]
fn tcp_listen_port_alone_is_accepted() {
    let raw = parse(
        r#"
project: demo
builder: { engine: podman }
servers:
  web: { host: 10.0.0.1 }
services:
  db:
    image: postgres:18
    servers: [web]
    proxy:
      targets:
        - port: 5432
          listen_port: 5432
          healthcheck:
            cmd: "pg_isready"
"#,
    );
    let result = validate_yaml(&raw);
    assert!(result.valid, "unexpected errors: {:?}", result.errors);
}

#[test]
fn cron_jobs_deserialize_with_defaults_and_overrides() {
    let config: Config = serde_yaml::from_str(
        r#"
project: demo
builder: { engine: podman }
servers:
  one: { host: 10.0.0.1 }
services:
  twitch:
    image: ghcr.io/example/twitch-sync:latest
    servers: [one]
    crons:
      sync-twitch:
        schedule: "7 */2 * * *"
        command: ["npm", "run", "sync:twitch"]
      remove-expired:
        schedule: "0 3 * * *"
        command: ["npm", "run", "remove-expired"]
        timezone: America/Denver
        timeout: 30m
        overlap: forbid
        missed_runs: skip
"#,
    )
    .unwrap();
    let service = &config.services["twitch"];
    assert_eq!(service.crons.len(), 2);

    let sync = &service.crons["sync-twitch"];
    assert_eq!(sync.timezone, "UTC");
    assert_eq!(sync.timeout, "1h");
    assert_eq!(
        sync.overlap,
        jiji_config::CronOverlap::Forbid,
        "overlap must default to forbid"
    );
    assert_eq!(
        sync.missed_runs,
        jiji_config::CronMissedRuns::Skip,
        "missed_runs must default to skip"
    );

    let remove_expired = &service.crons["remove-expired"];
    assert_eq!(remove_expired.timezone, "America/Denver");
    assert_eq!(remove_expired.timeout, "30m");
}

#[test]
fn cron_job_rejects_unknown_fields() {
    let raw = r#"
project: demo
builder: { engine: podman }
servers:
  one: { host: 10.0.0.1 }
services:
  twitch:
    image: ghcr.io/example/twitch-sync:latest
    servers: [one]
    crons:
      sync-twitch:
        schedule: "7 */2 * * *"
        command: ["npm", "run", "sync:twitch"]
        retriez: 3
"#;
    let err = serde_yaml::from_str::<Config>(raw).unwrap_err();
    assert!(
        err.to_string().contains("retriez"),
        "unexpected error: {err}"
    );
}

#[test]
fn cron_job_rejects_invalid_overlap_value() {
    let raw = r#"
project: demo
builder: { engine: podman }
servers:
  one: { host: 10.0.0.1 }
services:
  twitch:
    image: ghcr.io/example/twitch-sync:latest
    servers: [one]
    crons:
      sync-twitch:
        schedule: "7 */2 * * *"
        command: ["npm", "run", "sync:twitch"]
        overlap: queue
"#;
    assert!(serde_yaml::from_str::<Config>(raw).is_err());
}

#[test]
fn cron_job_rejects_invalid_missed_runs_value() {
    let raw = r#"
project: demo
builder: { engine: podman }
servers:
  one: { host: 10.0.0.1 }
services:
  twitch:
    image: ghcr.io/example/twitch-sync:latest
    servers: [one]
    crons:
      sync-twitch:
        schedule: "7 */2 * * *"
        command: ["npm", "run", "sync:twitch"]
        missed_runs: catch_up
"#;
    assert!(serde_yaml::from_str::<Config>(raw).is_err());
}

fn cron_config_yaml(cron_body: &str) -> String {
    format!(
        r#"
project: demo
builder: {{ engine: podman }}
servers:
  one: {{ host: 10.0.0.1 }}
services:
  twitch:
    image: ghcr.io/example/twitch-sync:latest
    servers: [one]
    crons:
      sync-twitch:
{cron_body}
"#
    )
}

#[test]
fn cron_job_rejects_schedule_with_seconds_field() {
    let raw = parse(&cron_config_yaml(
        "        schedule: \"* * * * * *\"\n        command: [\"npm\", \"run\", \"sync\"]",
    ));
    let result = validate_yaml(&raw);
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == "CRON_SCHEDULE_INVALID"));
}

#[test]
fn cron_job_rejects_schedule_alias() {
    let raw = parse(&cron_config_yaml(
        "        schedule: \"@daily\"\n        command: [\"npm\", \"run\", \"sync\"]",
    ));
    let result = validate_yaml(&raw);
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == "CRON_SCHEDULE_INVALID"));
}

#[test]
fn cron_job_rejects_malformed_schedule_field() {
    let raw = parse(&cron_config_yaml(
        "        schedule: \"nope * * * *\"\n        command: [\"npm\", \"run\", \"sync\"]",
    ));
    let result = validate_yaml(&raw);
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == "CRON_SCHEDULE_INVALID"));
}

#[test]
fn cron_job_accepts_valid_five_field_schedule() {
    let raw = parse(&cron_config_yaml(
        "        schedule: \"7 */2 * * *\"\n        command: [\"npm\", \"run\", \"sync\"]",
    ));
    let result = validate_yaml(&raw);
    assert!(result.valid, "unexpected errors: {:?}", result.errors);
}

#[test]
fn cron_job_rejects_invalid_timezone() {
    let raw = parse(&cron_config_yaml(
        "        schedule: \"0 3 * * *\"\n        command: [\"npm\", \"run\", \"sync\"]\n        timezone: Not/AZone",
    ));
    let result = validate_yaml(&raw);
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == "CRON_TIMEZONE_INVALID"));
}

#[test]
fn cron_job_rejects_empty_command() {
    let raw = parse(&cron_config_yaml(
        "        schedule: \"0 3 * * *\"\n        command: \"   \"",
    ));
    let result = validate_yaml(&raw);
    assert!(!result.valid);
    assert!(result.errors.iter().any(|e| e.code == "CRON_COMMAND_EMPTY"));
}

#[test]
fn cron_job_rejects_invalid_timeout() {
    let raw = parse(&cron_config_yaml(
        "        schedule: \"0 3 * * *\"\n        command: [\"npm\", \"run\", \"sync\"]\n        timeout: 0m",
    ));
    let result = validate_yaml(&raw);
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == "CRON_TIMEOUT_INVALID"));
}

#[test]
fn cron_job_rejects_invalid_name() {
    let raw = parse(
        r#"
project: demo
builder: { engine: podman }
servers:
  one: { host: 10.0.0.1 }
services:
  twitch:
    image: ghcr.io/example/twitch-sync:latest
    servers: [one]
    crons:
      Sync_Twitch:
        schedule: "0 3 * * *"
        command: ["npm", "run", "sync"]
"#,
    );
    let result = validate_yaml(&raw);
    assert!(!result.valid);
    assert!(result.errors.iter().any(|e| e.code == "CRON_NAME_INVALID"));
}

#[test]
fn cron_job_rejected_on_network_mode_service_dependent() {
    let raw = parse(
        r#"
project: demo
builder: { engine: podman }
servers:
  one: { host: 10.0.0.1 }
services:
  gluetun:
    image: gluetun
    servers: [one]
  qbittorrent:
    image: qbittorrent
    servers: [one]
    network_mode: service:gluetun
    crons:
      cleanup:
        schedule: "0 3 * * *"
        command: ["cleanup.sh"]
"#,
    );
    let result = validate_yaml(&raw);
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == "CRON_UNSUPPORTED_ON_NETWORK_MODE_SERVICE"));
}

#[test]
fn service_exceeding_max_crons_is_rejected() {
    let mut body = String::from(
        r#"
project: demo
builder: { engine: podman }
servers:
  one: { host: 10.0.0.1 }
services:
  twitch:
    image: ghcr.io/example/twitch-sync:latest
    servers: [one]
    crons:
"#,
    );
    for i in 0..33 {
        body.push_str(&format!(
            "      job-{i}:\n        schedule: \"0 3 * * *\"\n        command: [\"run\"]\n"
        ));
    }
    let result = validate_yaml(&parse(&body));
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == "TOO_MANY_CRONS_PER_SERVICE"));
}
