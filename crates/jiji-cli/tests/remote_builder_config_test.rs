use std::process::Command;

fn write_config(dir: &std::path::Path, builder_section: &str, extra: &str) -> std::path::PathBuf {
    let config = dir.join("deploy.yml");
    std::fs::write(
        &config,
        format!(
            r#"
project: demo
builder:
{builder_section}
servers:
  app:
    host: 127.0.0.1
    user: root
services:
  web:
    build: .
    servers: [app]
{extra}
"#
        ),
    )
    .expect("write config");
    config
}

fn run_build(config: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_jiji"))
        .arg("build")
        // These tests cover executor selection. A push would prepare the configured registry
        // first and make the result depend on Docker and local registry state on the test host.
        .arg("--no-push")
        .arg("-c")
        .arg(config)
        .output()
        .expect("run jiji build")
}

#[test]
fn invalid_remote_uri_surfaces_as_configuration_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = write_config(
        dir.path(),
        "  engine: docker\n  remote: ssh://user:pass@10.0.0.9\n",
        "",
    );
    let output = run_build(&config);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("password") && stderr.contains("builder.remote"),
        "stderr: {stderr}"
    );
}

#[test]
fn legacy_local_key_does_not_override_remote_inference() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = write_config(
        dir.path(),
        "  engine: docker\n  local: true\n  remote: ssh://build@127.0.0.1:1\n",
        "ssh:\n  user: fallback\n",
    );
    let output = run_build(&config);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Could not connect") && stderr.contains("build@127.0.0.1:1"),
        "stderr: {stderr}"
    );
}

#[test]
fn valid_remote_config_attempts_a_real_connection_with_the_resolved_identity() {
    // Port 1 on loopback: nothing listens there, so the connection is refused immediately
    // rather than timing out -- this only needs to prove config resolution reached a real
    // connection attempt with the right identity, not exercise the connection itself (that is
    // covered against a real in-process SSH server in remote_build_test.rs).
    let dir = tempfile::tempdir().expect("tempdir");
    let config = write_config(
        dir.path(),
        "  engine: docker\n  remote: ssh://build@127.0.0.1:1\n",
        "ssh:\n  user: fallback\n",
    );
    let output = run_build(&config);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Could not connect") && stderr.contains("build@127.0.0.1:1"),
        "stderr: {stderr}"
    );
}
