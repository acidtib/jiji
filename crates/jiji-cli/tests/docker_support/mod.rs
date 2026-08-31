//! Shared helpers for the Docker-in-Docker integration suite (`docker_*_test.rs`). These tests
//! run the real `jiji` binary over real SSH against `vm1`, a privileged container running its
//! own systemd and (once `jiji server setup` installs it) its own Podman -- see
//! `test/docker/compose.yml`. Nothing here mocks SSH; `crates/jiji-cli/tests/server_setup_test.rs`
//! is the in-process mock-SSH suite, this is its real-host counterpart.
//!
//! The compose stack itself is orchestrated externally (`.mise/tasks/test-docker`, or the
//! `docker-integration.yml` workflow), not by this module: these helpers assume `vm1` is already
//! up and healthy by the time a test calls them.

#![allow(dead_code)]

use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

pub const VM1_SSH_PORT: u16 = 2201;

/// Returns `true` (and prints a skip message) unless `JIJI_DOCKER_TESTS=1` is set. Call at the
/// top of every test in this suite and `return` early on `true`, so plain `cargo nextest run
/// --workspace` / `mise test` stays fast and dependency-free, matching the rest of the suite's
/// default behavior.
pub fn skip_unless_enabled() -> bool {
    if env::var("JIJI_DOCKER_TESTS").as_deref() == Ok("1") {
        false
    } else {
        eprintln!(
            "skipping docker integration test: set JIJI_DOCKER_TESTS=1 with `test/docker/compose.yml` up (see `mise test-docker`)"
        );
        true
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("resolve workspace root")
}

fn compose_file() -> PathBuf {
    workspace_root().join("test/docker/compose.yml")
}

/// Path to the private half of the keypair `test/docker/sshkeys` generates into the shared bind
/// mount. Only readable once the `sshkeys` service has actually started (compose brings it up
/// before `vm1` via `depends_on: condition: service_healthy`).
pub fn ssh_key_path() -> PathBuf {
    workspace_root().join("test/docker/.shared/ssh/id_ed25519")
}

fn compose(args: &[&str]) -> Output {
    Command::new("docker")
        .arg("compose")
        .arg("-f")
        .arg(compose_file())
        .args(args)
        .output()
        .expect("run docker compose")
}

/// Runs a command inside `vm1` via `docker compose exec`, for asserting real post-`jiji`-run
/// state (`systemctl is-active ...`, `wg show ...`, `podman info`, ...).
pub fn exec_vm1(cmd: &[&str]) -> Output {
    let mut args = vec!["exec", "-T", "vm1"];
    args.extend_from_slice(cmd);
    compose(&args)
}

pub fn exec_vm1_ok(cmd: &[&str]) -> bool {
    exec_vm1(cmd).status.success()
}

pub fn exec_vm1_stdout(cmd: &[&str]) -> String {
    let output = exec_vm1(cmd);
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Polls `vm1` over SSH until it accepts a trivial command, since `depends_on:
/// condition: service_healthy` only proves compose brought the container up, not that systemd
/// has finished booting sshd inside it.
pub fn wait_for_vm1_ready(timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if exec_vm1_ok(&["true"]) {
            return;
        }
        if Instant::now() >= deadline {
            panic!("vm1 did not become ready within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Writes a `deploy.yml` fixture pointing at the real `vm1` container over its published SSH
/// port, using the shared test keypair and `root` (matching how jiji actually connects to a
/// fresh, unprovisioned host: AGENTS.md's "a host's trust boundary is 'this file was installed by
/// root'"). `builder.engine: podman` drives `jiji server setup` to install real static Podman on
/// vm1, the same install path a real Ubuntu droplet goes through.
pub fn write_config(dir: &Path, project: &str) -> PathBuf {
    let config_path = dir.join("deploy.yml");
    std::fs::write(
        &config_path,
        format!(
            r#"
project: {project}
builder:
  engine: podman
servers:
  vm1:
    host: 127.0.0.1
    port: {port}
    keys:
      - {key_path}
services: {{}}
ssh:
  user: root
  keys_only: true
"#,
            port = VM1_SSH_PORT,
            key_path = ssh_key_path().display(),
        ),
    )
    .expect("write test deploy.yml");
    config_path
}

pub fn run_jiji(config_path: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_jiji"))
        .args(args)
        .arg("-c")
        .arg(config_path)
        .output()
        .expect("run jiji")
}
