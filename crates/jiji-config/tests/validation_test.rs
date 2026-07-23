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
fn service_with_no_hosts_reports_no_hosts() {
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
        .any(|e| e.code == "NO_HOSTS" && e.path == "services.app.hosts"));
}

#[test]
fn service_host_referencing_undefined_server_reports_undefined_server() {
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
    hosts:
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
    hosts:
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
    hosts:
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
    hosts: [web]
network:
  management_cidr: 198.18.0.0/16
  container_cidr: 100.64.0.0/10
"#,
    );
    let result = validate_yaml(&raw);
    assert!(result.valid, "unexpected errors: {:?}", result.errors);
}
