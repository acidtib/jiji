use std::os::unix::fs::PermissionsExt;
use std::process::Command;

fn fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("deploy.yml");
    std::fs::write(
        &config,
        r#"
project: demo
builder:
  engine: docker
  registry:
    type: local
    port: 31270
servers:
  app:
    host: 127.0.0.1
    user: root
services:
  web:
    image: example/web:latest
    hosts: [app]
"#,
    )
    .expect("write config");

    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).expect("create bin");
    let docker = bin.join("docker");
    std::fs::write(
        &docker,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$JIJI_TEST_LOG"
if [ "$1" = "container" ] && [ "$2" = "inspect" ]; then
  printf '%s\n' "$JIJI_TEST_INSPECT"
fi
"#,
    )
    .expect("write docker");
    let mut permissions = std::fs::metadata(&docker).expect("metadata").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&docker, permissions).expect("chmod");
    (dir, config, bin)
}

fn run(
    config: &std::path::Path,
    bin: &std::path::Path,
    log: &std::path::Path,
    inspect: &str,
    extra: &[&str],
) -> std::process::Output {
    let current_path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![bin.to_path_buf()];
    paths.extend(std::env::split_paths(&current_path));
    let mut command = Command::new(env!("CARGO_BIN_EXE_jiji"));
    command
        .arg("registry")
        .arg("teardown")
        .arg("-c")
        .arg(config)
        .env("PATH", std::env::join_paths(paths).expect("join PATH"))
        .env("JIJI_TEST_LOG", log)
        .env("JIJI_TEST_INSPECT", inspect);
    command.args(extra);
    command.output().expect("run registry teardown")
}

#[test]
fn dry_run_inspects_but_does_not_remove() {
    let (dir, config, bin) = fixture();
    let log = dir.path().join("commands.log");
    let output = run(
        &config,
        &bin,
        &log,
        "true|registry|31270|true",
        &["--dry-run"],
    );
    assert!(output.status.success());
    let commands = std::fs::read_to_string(log).expect("read log");
    assert!(commands.contains("container inspect"));
    assert!(!commands.contains("container rm"));
}

#[test]
fn ownership_or_port_mismatch_blocks_removal() {
    let (dir, config, bin) = fixture();
    let log = dir.path().join("commands.log");
    let output = run(&config, &bin, &log, "false|registry|31270|true", &["--yes"]);
    assert!(!output.status.success());
    let commands = std::fs::read_to_string(log).expect("read log");
    assert!(!commands.contains("container rm"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("not Jiji's registry"));
}

#[test]
fn confirmed_teardown_removes_only_the_exact_owned_container() {
    let (dir, config, bin) = fixture();
    let log = dir.path().join("commands.log");
    let output = run(&config, &bin, &log, "true|registry|31270|false", &["--yes"]);
    assert!(output.status.success());
    let commands = std::fs::read_to_string(log).expect("read log");
    assert!(commands.contains("container rm -f jiji-registry"));
}
