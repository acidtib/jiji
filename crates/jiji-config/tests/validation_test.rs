use jiji_config::{validate_yaml, TEMPLATE};

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
fn builder_local_true_with_remote_reports_mode_conflict() {
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
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == "BUILDER_MODE_CONFLICT" && e.path == "builder.remote"));
}

#[test]
fn builder_local_false_without_remote_reports_remote_required() {
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
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == "BUILDER_REMOTE_REQUIRED" && e.path == "builder.remote"));
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
