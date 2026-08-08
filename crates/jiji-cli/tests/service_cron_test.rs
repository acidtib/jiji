//! Integration tests for the `jiji service cron` command skeleton (Phase 1 of
//! `plans/service-cron.md`: configuration + CLI surface only, no scheduler/agent backend yet).
//! None of these commands open an SSH connection (`list` is purely local; `status`/`logs`/`run`
//! validate their target and then report the not-yet-implemented gap), so no mock SSH server is
//! needed here, unlike `service_prune_test.rs`.

use std::process::Command;

fn config_yaml() -> &'static str {
    r#"
project: demo
builder: { engine: podman }
servers:
  app: { host: 10.0.0.1 }
services:
  web:
    image: nginx
    servers: [app]
  worker:
    image: worker
    servers: [app]
    crons:
      sync-data:
        schedule: "0 3 * * *"
        command: ["sync"]
      cleanup:
        schedule: "30 4 * * *"
        command: ["cleanup"]
  another-worker:
    image: worker
    servers: [app]
    crons:
      backup:
        schedule: "0 5 * * *"
        command: ["backup"]
"#
}

fn write_config(dir: &std::path::Path) -> std::path::PathBuf {
    let config_path = dir.join("deploy.yml");
    std::fs::write(&config_path, config_yaml()).expect("write test deploy.yml");
    config_path
}

fn run_jiji(config_path: &std::path::Path, args: &[&str]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_jiji"));
    command.arg("service").arg("cron");
    for arg in args {
        command.arg(arg);
    }
    command.arg("-c").arg(config_path);
    command.output().expect("run jiji service cron")
}

#[test]
fn help_lists_the_four_subcommands() {
    let output = Command::new(env!("CARGO_BIN_EXE_jiji"))
        .args(["service", "cron", "--help"])
        .output()
        .expect("run jiji service cron --help");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    for expected in ["list", "status", "logs", "run"] {
        assert!(
            stdout.contains(expected),
            "stdout missing '{expected}': {stdout}"
        );
    }
}

#[test]
fn list_reports_no_cron_jobs_for_a_service_without_any() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = write_config(dir.path());

    let output = run_jiji(&config_path, &["list", "-S", "web"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        stdout.contains("No cron jobs are configured"),
        "stdout: {stdout}"
    );
}

#[test]
fn list_reports_configured_jobs_as_not_deployed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = write_config(dir.path());

    let output = run_jiji(&config_path, &["list", "-S", "worker"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        stdout.contains("worker sync-data:")
            && stdout.contains("schedule=\"0 3 * * *\"")
            && stdout.contains("state=not-deployed"),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("worker cleanup:"), "stdout: {stdout}");
    assert!(
        !stdout.contains("another-worker"),
        "the -S filter should have excluded another-worker: {stdout}"
    );
}

#[test]
fn status_reports_not_implemented_for_a_matched_service() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = write_config(dir.path());

    let output = run_jiji(&config_path, &["status", "-S", "worker"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not implemented yet"), "stderr: {stderr}");
}

#[test]
fn status_reports_no_match_when_filter_selects_no_cron_service() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = write_config(dir.path());

    let output = run_jiji(&config_path, &["status", "-S", "web"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("No service with cron jobs matched"),
        "stderr: {stderr}"
    );
}

#[test]
fn logs_reports_unknown_cron_name_with_available_names() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = write_config(dir.path());

    let output = run_jiji(&config_path, &["logs", "nope", "-S", "worker"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("has no cron named 'nope'") && stderr.contains("cleanup, sync-data"),
        "stderr: {stderr}"
    );
}

#[test]
fn logs_rejects_a_filter_matching_multiple_cron_services() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = write_config(dir.path());

    let output = run_jiji(
        &config_path,
        &["logs", "backup", "-S", "worker,another-worker"],
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("matched 2 services with cron jobs"),
        "stderr: {stderr}"
    );
}

#[test]
fn logs_reports_not_implemented_for_a_valid_target() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = write_config(dir.path());

    let output = run_jiji(&config_path, &["logs", "sync-data", "-S", "worker"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not implemented yet")
            && stderr.contains("sync-data")
            && stderr.contains("worker"),
        "stderr: {stderr}"
    );
}

#[test]
fn logs_rejects_follow_combined_with_run_id() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = write_config(dir.path());

    let output = run_jiji(
        &config_path,
        &[
            "logs",
            "sync-data",
            "-S",
            "worker",
            "--run",
            "abc123",
            "--follow",
        ],
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--follow reads the active run"),
        "stderr: {stderr}"
    );
}

#[test]
fn run_reports_not_implemented_for_a_valid_target() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = write_config(dir.path());

    let output = run_jiji(&config_path, &["run", "backup", "-S", "another-worker"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not implemented yet") && stderr.contains("backup"),
        "stderr: {stderr}"
    );
}

#[test]
fn run_rejects_unknown_cron_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = write_config(dir.path());

    let output = run_jiji(&config_path, &["run", "nope", "-S", "another-worker"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("has no cron named 'nope'"),
        "stderr: {stderr}"
    );
}
