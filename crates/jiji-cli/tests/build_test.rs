use std::os::unix::fs::PermissionsExt;
use std::process::Command;

/// Proves `BuildExecutor::Local`'s behavior is unchanged after the `build_plan::build_one` /
/// `BuildExecutor` refactor: no CLI-level test of `jiji build` existed before this. Uses the
/// same PATH-shim fake-engine pattern as `registry_teardown_test.rs`/`registry_auth_test.rs`
/// (a local, non-SSH scope), not the russh mock-server pattern `remote_build_test.rs` uses for
/// the remote scope.
fn write_config(dir: &std::path::Path, registry_section: &str) -> std::path::PathBuf {
    let config = dir.join("deploy.yml");
    std::fs::write(
        &config,
        format!(
            r#"
project: demo
builder:
  engine: docker
  registry:
{registry_section}
servers:
  app:
    host: 127.0.0.1
    user: root
services:
  web:
    build: .
    servers: [app]
"#
        ),
    )
    .expect("write config");
    config
}

fn write_fake_docker(dir: &std::path::Path) -> std::path::PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir(&bin).expect("create bin");
    let docker = bin.join("docker");
    std::fs::write(
        &docker,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$JIJI_TEST_LOG"
exit 0
"#,
    )
    .expect("write docker");
    let mut permissions = std::fs::metadata(&docker).expect("metadata").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&docker, permissions).expect("chmod");
    bin
}

fn run_build(
    config: &std::path::Path,
    bin: &std::path::Path,
    log: &std::path::Path,
    extra: &[&str],
) -> std::process::Output {
    let current_path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![bin.to_path_buf()];
    paths.extend(std::env::split_paths(&current_path));
    let mut command = Command::new(env!("CARGO_BIN_EXE_jiji"));
    command
        .arg("build")
        .arg("-c")
        .arg(config)
        .env("PATH", std::env::join_paths(paths).expect("join PATH"))
        .env("JIJI_TEST_LOG", log);
    command.args(extra);
    command.output().expect("run jiji build")
}

#[test]
fn single_arch_no_push_build_renders_the_expected_local_docker_command() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = write_config(dir.path(), "");
    let bin = write_fake_docker(dir.path());
    let log = dir.path().join("log.txt");
    std::fs::write(&log, "").expect("create log");

    let output = run_build(&config, &bin, &log, &["--version", "v1.2.3", "--no-push"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let logged = std::fs::read_to_string(&log).expect("read log");
    let build_line = logged
        .lines()
        .find(|line| line.starts_with("build "))
        .expect("expected a logged `docker build` invocation");
    assert_eq!(
        build_line,
        "build -f Dockerfile -t localhost:31270/demo-web:v1.2.3 -t localhost:31270/demo-web:latest ."
    );
    assert!(
        !logged.lines().any(|line| line.starts_with("push ")),
        "no push should run with --no-push, log: {logged}"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Executor: local"), "stdout: {stdout}");
    assert!(stdout.contains("built locally"), "stdout: {stdout}");
}

#[test]
fn build_arg_references_use_the_merged_service_environment() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("deploy.yml");
    std::fs::write(
        &config,
        r#"
project: demo
builder: { engine: docker }
environment:
  clear:
    NEXT_PUBLIC_DOMAIN: shared.example.com
servers:
  app: { host: 127.0.0.1 }
services:
  web:
    build:
      context: .
      args:
        NEXT_PUBLIC_DOMAIN: NEXT_PUBLIC_DOMAIN
    servers: [app]
    environment:
      clear:
        NEXT_PUBLIC_DOMAIN: service.example.com
"#,
    )
    .expect("write config");
    let bin = write_fake_docker(dir.path());
    let log = dir.path().join("log.txt");
    std::fs::write(&log, "").expect("create log");

    let output = run_build(&config, &bin, &log, &["--version", "v1", "--no-push"]);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let logged = std::fs::read_to_string(&log).expect("read log");
    let build_line = logged
        .lines()
        .find(|line| line.starts_with("build "))
        .expect("build command");
    assert!(
        build_line.contains("--build-arg NEXT_PUBLIC_DOMAIN=service.example.com"),
        "log: {logged}"
    );
}

#[test]
fn single_arch_push_build_renders_build_then_push() {
    // A remote registry (rather than local) avoids `ensure_local_registry`'s real TCP
    // readiness wait, which a fake `docker` script can't meaningfully satisfy -- this still
    // exercises the exact same `BuildExecutor::Local` build-then-push sequence.
    let dir = tempfile::tempdir().expect("tempdir");
    let config = write_config(dir.path(), "    server: registry.example.com\n");
    let bin = write_fake_docker(dir.path());
    let log = dir.path().join("log.txt");
    std::fs::write(&log, "").expect("create log");

    let output = run_build(&config, &bin, &log, &["--version", "v1.2.3"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let logged = std::fs::read_to_string(&log).expect("read log");
    let lines: Vec<&str> = logged
        .lines()
        .filter(|line| line.starts_with("build ") || line.starts_with("push "))
        .collect();
    assert_eq!(
        lines,
        vec![
            "build -f Dockerfile -t registry.example.com/demo-web:v1.2.3 -t registry.example.com/demo-web:latest .",
            "push registry.example.com/demo-web:v1.2.3",
            "push registry.example.com/demo-web:latest",
        ],
        "log: {logged}"
    );
}

fn write_multi_arch_config(dir: &std::path::Path) -> std::path::PathBuf {
    let config = dir.join("deploy.yml");
    std::fs::write(
        &config,
        r#"
project: demo
builder:
  engine: docker
  registry:
    server: registry.example.com
servers:
  app1:
    host: 127.0.0.1
    user: root
  app2:
    host: 127.0.0.1
    user: root
    arch: arm64
services:
  web:
    build: .
    servers: [app1, app2]
"#,
    )
    .expect("write config");
    config
}

#[test]
fn multi_arch_docker_build_uses_the_project_scoped_builder_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = write_multi_arch_config(dir.path());
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).expect("create bin");
    let docker = bin.join("docker");
    std::fs::write(
        &docker,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$JIJI_TEST_LOG"
if [ "$1" = "buildx" ] && [ "$2" = "inspect" ]; then exit 1; fi
exit 0
"#,
    )
    .expect("write docker");
    let mut permissions = std::fs::metadata(&docker).expect("metadata").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&docker, permissions).expect("chmod");
    let log = dir.path().join("log.txt");
    std::fs::write(&log, "").expect("create log");

    let output = run_build(&config, &bin, &log, &["--version", "v1"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let logged = std::fs::read_to_string(&log).expect("read log");
    assert!(
        logged.contains("buildx create --name jiji-builder-demo"),
        "log: {logged}"
    );
    let build_line = logged
        .lines()
        .find(|line| line.starts_with("buildx build "))
        .expect("expected a logged `docker buildx build` invocation");
    assert!(
        build_line.contains("--builder jiji-builder-demo"),
        "{build_line}"
    );
    assert!(
        build_line.contains("--platform linux/amd64,linux/arm64"),
        "no requested platform should be dropped: {build_line}"
    );
}

#[test]
fn multi_arch_docker_tolerates_a_concurrent_builder_create_race() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = write_multi_arch_config(dir.path());
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).expect("create bin");
    let marker = dir.path().join("builder-exists");
    let docker = bin.join("docker");
    std::fs::write(
        &docker,
        format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> "$JIJI_TEST_LOG"
if [ "$1" = "buildx" ] && [ "$2" = "inspect" ]; then
  if [ -f "{marker}" ]; then exit 0; else exit 1; fi
fi
if [ "$1" = "buildx" ] && [ "$2" = "create" ]; then
  touch "{marker}"
  exit 1
fi
exit 0
"#,
            marker = marker.display(),
        ),
    )
    .expect("write docker");
    let mut permissions = std::fs::metadata(&docker).expect("metadata").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&docker, permissions).expect("chmod");
    let log = dir.path().join("log.txt");
    std::fs::write(&log, "").expect("create log");

    let output = run_build(&config, &bin, &log, &["--version", "v1"]);
    assert!(
        output.status.success(),
        "a lost create race should not fail the build once the builder exists: stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let logged = std::fs::read_to_string(&log).expect("read log");
    let inspect_count = logged
        .lines()
        .filter(|line| line.starts_with("buildx inspect"))
        .count();
    assert_eq!(
        inspect_count, 2,
        "expected an initial inspect and a retry inspect after the lost race: {logged}"
    );
    assert!(
        logged.lines().any(|line| line.starts_with("buildx build ")),
        "the build must still proceed after the race resolves: {logged}"
    );
}
