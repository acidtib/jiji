use std::process::Command;

fn write_config(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("deploy.yml");
    std::fs::write(
        &path,
        r#"
project: proxy-test
builder:
  engine: docker
servers:
  web1:
    host: 192.0.2.1
  web2:
    host: 192.0.2.2
services: {}
ssh:
  user: tester
"#,
    )
    .expect("write test config");
    path
}

#[test]
fn restart_rejects_service_filter_before_loading_configuration() {
    let output = Command::new(env!("CARGO_BIN_EXE_jiji"))
        .args(["-S", "web", "-c", "/does/not/exist", "proxy", "restart"])
        .output()
        .expect("run proxy restart");

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("does not accept -S/--services"));
}

#[test]
fn logs_rejects_service_filter_before_loading_configuration() {
    let output = Command::new(env!("CARGO_BIN_EXE_jiji"))
        .args(["-S", "web", "-c", "/does/not/exist", "proxy", "logs"])
        .output()
        .expect("run proxy logs");

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("does not accept -S/--services"));
}

#[test]
fn follow_rejects_multiple_hosts_before_connecting() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let config = write_config(dir.path());
    let output = Command::new(env!("CARGO_BIN_EXE_jiji"))
        .arg("-c")
        .arg(config)
        .args(["proxy", "logs", "--follow"])
        .output()
        .expect("run proxy logs");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("requires exactly one host"), "{stderr}");
    assert!(stderr.contains("web1, web2"), "{stderr}");
}
