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

/// Writes a `{dir}/.jiji/deploy.yml` fixture (the real layout `jiji init` itself writes, not a
/// bare `deploy.yml` at `dir`'s root) pointing at the real `vm1` container over its published SSH
/// port, using the shared test keypair and `root` (matching how jiji actually connects to a
/// fresh, unprovisioned host: AGENTS.md's "a host's trust boundary is 'this file was installed by
/// root'"). `builder.engine: podman` drives `jiji server setup` to install real static Podman on
/// vm1, the same install path a real Ubuntu droplet goes through.
pub fn write_config(dir: &Path, project: &str) -> PathBuf {
    let jiji_dir = dir.join(".jiji");
    std::fs::create_dir_all(&jiji_dir).expect("create .jiji dir");
    let config_path = jiji_dir.join("deploy.yml");
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

/// Host header the proxied fixture service routes on. No real DNS involved: tests reach vm1's
/// published proxy port directly and set this as the `Host` header themselves, the same way
/// Kamal's own integration suite constructs requests against its load balancer.
pub const PROXY_TEST_HOST: &str = "app.jiji.test";
pub const PROXY_HTTP_PORT: u16 = 8080;

/// Same as `write_config`, plus one service (a stock `nginx:alpine`, not `build:` -- see
/// `write_config_with_build_service` for that) proxied over HTTP on `PROXY_TEST_HOST`,
/// health-checked at `/` before `jiji deploy` will ever route traffic to it.
pub fn write_config_with_proxied_web_service(dir: &Path, project: &str) -> PathBuf {
    let jiji_dir = dir.join(".jiji");
    std::fs::create_dir_all(&jiji_dir).expect("create .jiji dir");
    let config_path = jiji_dir.join("deploy.yml");
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
services:
  web:
    image: nginx:alpine
    servers: [vm1]
    proxy:
      port: 80
      ssl: false
      hosts: [{proxy_host}]
      healthcheck:
        path: /
ssh:
  user: root
  keys_only: true
"#,
            port = VM1_SSH_PORT,
            key_path = ssh_key_path().display(),
            proxy_host = PROXY_TEST_HOST,
        ),
    )
    .expect("write test deploy.yml");
    config_path
}

/// Content the `docker_build_deploy_test` fixture's image bakes into its own index page, so a
/// passing HTTP assertion proves the image was actually rebuilt and pulled through, not that a
/// stale/cached image was already sitting there.
pub const BUILD_TEST_MARKER: &str = "jiji-docker-build-test";

/// Distinct from `PROXY_TEST_HOST`: this test runs under a different project name but on the
/// same shared, host-global jiji-proxy, so giving it its own Host header avoids depending on
/// `docker_deploy_test`'s own teardown having already cleared its route by the time this runs.
pub const BUILD_PROXY_TEST_HOST: &str = "build.jiji.test";

/// A service built from a real local Dockerfile and pushed through jiji's own build pipeline: no
/// `registry:` block, so `Registry::is_local()` is true and jiji manages its own local registry
/// container, builds and pushes to it, then opens a reverse SSH tunnel to vm1 during `jiji deploy
/// --build` so vm1's Podman can pull from `localhost:{port}` as if the registry were local to it
/// (see `crates/jiji-cli/src/registry.rs` and `commands/deploy.rs`'s `start_reverse_forward`
/// call). Config lives at `{dir}/.jiji/deploy.yml`, not `{dir}/deploy.yml`: `build: ./app` resolves
/// relative to the project root, which jiji derives as two directories above the config file
/// (`env_resolution::project_root_from_config_path`) -- that only lines up with `{dir}/app` when
/// the config actually sits under a `.jiji/` subdirectory, the real-world layout `jiji init`
/// itself writes.
pub fn write_config_with_build_service(dir: &Path, project: &str) -> PathBuf {
    let jiji_dir = dir.join(".jiji");
    std::fs::create_dir_all(&jiji_dir).expect("create .jiji dir");

    let app_dir = dir.join("app");
    std::fs::create_dir_all(&app_dir).expect("create app dir");
    std::fs::write(
        app_dir.join("Dockerfile"),
        format!(
            "FROM nginx:alpine\nRUN echo '{BUILD_TEST_MARKER}' > /usr/share/nginx/html/index.html\n"
        ),
    )
    .expect("write test app Dockerfile");

    let config_path = jiji_dir.join("deploy.yml");
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
services:
  web:
    build: ./app
    servers: [vm1]
    proxy:
      port: 80
      ssl: false
      hosts: [{proxy_host}]
      healthcheck:
        path: /
ssh:
  user: root
  keys_only: true
"#,
            port = VM1_SSH_PORT,
            key_path = ssh_key_path().display(),
            proxy_host = BUILD_PROXY_TEST_HOST,
        ),
    )
    .expect("write test deploy.yml");
    config_path
}

/// A bare-bones HTTP/1.0 GET over a raw `TcpStream`, since `jiji-cli`'s `reqwest` dependency
/// isn't built with the `blocking` feature and pulling in a whole async runtime just to check one
/// status code isn't worth it here. Returns `(status_code, body)`.
fn http_get(port: u16, host_header: &str, path: &str) -> std::io::Result<(u16, String)> {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {host_header}\r\nConnection: close\r\n\r\n"
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;

    let status_line = response
        .lines()
        .next()
        .ok_or_else(|| std::io::Error::other("empty HTTP response"))?;
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| std::io::Error::other(format!("unparseable status line: {status_line}")))?;
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default();
    Ok((status_code, body))
}

/// Polls `PROXY_HTTP_PORT` until it returns HTTP 200, since a successful `jiji deploy` still
/// leaves a short window before jiji-proxy's route is actually reachable end-to-end.
pub fn wait_for_http_ok(host_header: &str, path: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let outcome = http_get(PROXY_HTTP_PORT, host_header, path);
        if let Ok((200, body)) = outcome {
            return body;
        }
        if Instant::now() >= deadline {
            let reason = match outcome {
                Ok((status, _)) => format!("got HTTP {status}"),
                Err(err) => err.to_string(),
            };
            panic!(
                "http://{host_header}{path} via 127.0.0.1:{PROXY_HTTP_PORT} did not return 200 within {timeout:?}: {reason}"
            );
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}
