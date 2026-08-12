use jiji_config::{BuildValue, Config};

#[test]
fn build_context_defaults_to_project_root_when_omitted() {
    let yaml = r#"
project: demo
builder:
  engine: podman
servers:
  web:
    host: 10.0.0.1
services:
  app:
    image: nginx:latest
    servers: [web]
    build:
      dockerfile: Dockerfile
"#;
    let config: Config =
        serde_yaml::from_str(yaml).expect("config with context omitted should parse");
    let service = config.services.get("app").expect("service 'app'");
    match service.build.as_ref().expect("build config present") {
        BuildValue::Detailed(build) => assert_eq!(build.context, "."),
        BuildValue::Context(_) => panic!("expected the detailed build form"),
    }
}

#[test]
fn build_context_explicit_value_is_not_overwritten() {
    let yaml = r#"
project: demo
builder:
  engine: podman
servers:
  web:
    host: 10.0.0.1
services:
  app:
    image: nginx:latest
    servers: [web]
    build:
      context: ./api
      dockerfile: Dockerfile
"#;
    let config: Config = serde_yaml::from_str(yaml).expect("config should parse");
    let service = config.services.get("app").expect("service 'app'");
    match service.build.as_ref().expect("build config present") {
        BuildValue::Detailed(build) => assert_eq!(build.context, "./api"),
        BuildValue::Context(_) => panic!("expected the detailed build form"),
    }
}
