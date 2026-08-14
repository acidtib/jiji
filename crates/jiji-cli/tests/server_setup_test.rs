//! Integration tests for `jiji server setup`, run as a real subprocess against a real, in-process
//! SSH server (mirroring `jiji-ssh`'s own `session_test.rs` pattern), so the full config-load ->
//! connect -> engine-check/install path is exercised without touching a real host.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jiji_agent::AgentPaths;
use rand::rng;
use russh::keys::ssh_key::LineEnding;
use russh::keys::{Algorithm, PrivateKey, PublicKey};
use russh::server::{self, Auth, ChannelOpenHandle, Server as _, Session};
use russh::{Channel, ChannelId};
use tokio::net::TcpListener;

/// Canned response for a specific command. `None` for a field falls back to the server's
/// default (success, no output).
#[derive(Clone, Default)]
struct CannedResponse {
    success: bool,
    stdout: String,
    stderr: String,
}

/// (command, stdin bytes) pairs recorded per completed channel, so a test can inspect exactly
/// what a piped-input command (e.g. `install -m 0600 /dev/stdin membership-update.json`) sent.
type ReceivedStdin = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

#[derive(Clone)]
struct TestServer {
    authorized_key: PublicKey,
    responses: HashMap<String, CannedResponse>,
    pending: Arc<Mutex<HashMap<ChannelId, String>>>,
    received: Arc<Mutex<Vec<String>>>,
    stdin: Arc<Mutex<HashMap<ChannelId, Vec<u8>>>>,
    received_stdin: ReceivedStdin,
}

impl server::Server for TestServer {
    type Handler = Self;

    fn new_client(&mut self, _peer_addr: Option<SocketAddr>) -> Self {
        self.clone()
    }
}

impl server::Handler for TestServer {
    type Error = russh::Error;

    async fn auth_publickey(&mut self, _user: &str, key: &PublicKey) -> Result<Auth, Self::Error> {
        Ok(if *key == self.authorized_key {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<server::Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.pending
            .lock()
            .expect("pending mutex poisoned")
            .insert(channel, String::from_utf8_lossy(data).into_owned());
        session.channel_success(channel)?;
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.stdin
            .lock()
            .expect("stdin mutex poisoned")
            .entry(channel)
            .or_default()
            .extend_from_slice(data);
        Ok(())
    }

    // Deferring to EOF avoids a race with the client's pipelined exec+eof messages, same as
    // jiji-ssh's own test server.
    async fn channel_eof(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let command = self
            .pending
            .lock()
            .expect("pending mutex poisoned")
            .remove(&channel)
            .unwrap_or_default();
        self.received
            .lock()
            .expect("received mutex poisoned")
            .push(command.clone());
        if let Some(stdin) = self
            .stdin
            .lock()
            .expect("stdin mutex poisoned")
            .remove(&channel)
        {
            self.received_stdin
                .lock()
                .expect("received_stdin mutex poisoned")
                .push((command.clone(), stdin));
        }

        // Commands with no canned response (e.g. the many install-step shell commands a test
        // doesn't care about individually) succeed with empty output by default.
        let response = if command.contains("if test -L ") && command.contains("/current") {
            success("-\n")
        } else if command.contains("inspect jiji-proxy --format '{{.State.Status}}'") {
            success("running\n")
        } else {
            self.responses
                .get(&command)
                .cloned()
                .unwrap_or_else(|| success(""))
        };

        if !response.stdout.is_empty() {
            session.data(channel, response.stdout)?;
        }
        if !response.stderr.is_empty() {
            session.extended_data(channel, 1, response.stderr)?;
        }
        session.exit_status_request(channel, if response.success { 0 } else { 1 })?;
        session.eof(channel)?;
        session.close(channel)?;
        Ok(())
    }
}

async fn spawn_test_server(
    authorized_key: PublicKey,
    responses: HashMap<String, CannedResponse>,
) -> (SocketAddr, Arc<Mutex<Vec<String>>>, ReceivedStdin) {
    let config = Arc::new(server::Config {
        auth_rejection_time: Duration::from_millis(50),
        keys: vec![PrivateKey::random(&mut rng(), Algorithm::Ed25519).expect("generate host key")],
        ..Default::default()
    });

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test listener");
    let addr = listener.local_addr().expect("read listener addr");

    let received = Arc::new(Mutex::new(Vec::new()));
    let received_stdin = Arc::new(Mutex::new(Vec::new()));
    let mut test_server = TestServer {
        authorized_key,
        responses,
        pending: Arc::new(Mutex::new(HashMap::new())),
        received: Arc::clone(&received),
        stdin: Arc::new(Mutex::new(HashMap::new())),
        received_stdin: Arc::clone(&received_stdin),
    };

    tokio::spawn(async move {
        let _ = test_server.run_on_socket(config, &listener).await;
        drop(listener);
    });

    (addr, received, received_stdin)
}

fn success(stdout: &str) -> CannedResponse {
    CannedResponse {
        success: true,
        stdout: stdout.to_string(),
        stderr: String::new(),
    }
}

fn failure() -> CannedResponse {
    CannedResponse {
        success: false,
        stdout: String::new(),
        stderr: String::new(),
    }
}

fn add_network_setup_responses(responses: &mut HashMap<String, CannedResponse>) {
    responses.insert("id -u".to_string(), success("0\n"));
    let dir = format!(
        "/etc/jiji/network/{}",
        jiji_network::systemd_unit_slug("testproject")
    );
    let command = format!("test -s {dir}/public.key && cat {dir}/public.key");
    responses.insert(command.clone(), success("test-wireguard-public-key\n"));
    responses.insert(
        format!("{command}#2"),
        success("test-wireguard-public-key\n"),
    );
    responses.insert(
        format!("cat {dir}/public.key"),
        success("test-wireguard-public-key\n"),
    );
}

/// A `JIJI_AGENT_BINARY` override candidate that actually runs: `resolve_agent_binary_source`
/// now execs `{path} version` locally before trusting a discovered binary (see
/// `agent_distribution.rs`), so a placeholder that isn't a real executable is rejected as
/// unparseable rather than silently uploaded, unlike before that check existed.
fn write_fake_local_agent_binary(dir: &std::path::Path, version: &str) -> std::path::PathBuf {
    let path = dir.join("fake-jiji-agent");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nif [ \"$1\" = version ]; then printf '%s\\n' '{version}'; fi\nexit 0\n"
        ),
    )
    .expect("write fake agent binary");
    let mut permissions = std::fs::metadata(&path)
        .expect("read fake agent binary metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).expect("chmod fake agent binary");
    path
}

fn write_config(
    dir: &std::path::Path,
    addr: SocketAddr,
    key_path: &std::path::Path,
) -> std::path::PathBuf {
    let config_path = dir.join("deploy.yml");
    std::fs::write(
        &config_path,
        format!(
            r#"
project: testproject
builder:
  engine: docker
servers:
  web1:
    host: {ip}
    port: {port}
    keys:
      - {key_path}
services: {{}}
ssh:
  user: tester
  keys_only: true
"#,
            ip = addr.ip(),
            port = addr.port(),
            key_path = key_path.display(),
        ),
    )
    .expect("write test deploy.yml");
    config_path
}

fn write_config_with_web_service(
    dir: &std::path::Path,
    addr: SocketAddr,
    key_path: &std::path::Path,
) -> std::path::PathBuf {
    let config_path = dir.join("deploy.yml");
    std::fs::write(
        &config_path,
        format!(
            r#"
project: testproject
builder:
  engine: docker
servers:
  web1:
    host: {ip}
    port: {port}
    keys:
      - {key_path}
services:
  web:
    image: nginx:alpine
    servers: [web1]
ssh:
  user: tester
  keys_only: true
"#,
            ip = addr.ip(),
            port = addr.port(),
            key_path = key_path.display(),
        ),
    )
    .expect("write test deploy.yml");
    config_path
}

fn write_two_server_config(
    dir: &std::path::Path,
    first: SocketAddr,
    second: SocketAddr,
    key_path: &std::path::Path,
) -> std::path::PathBuf {
    let config_path = dir.join("deploy.yml");
    std::fs::write(
        &config_path,
        format!(
            r#"
project: testproject
builder:
  engine: docker
servers:
  existing:
    host: {first_ip}
    port: {first_port}
    keys:
      - {key_path}
  new-host:
    host: {second_ip}
    port: {second_port}
    keys:
      - {key_path}
services: {{}}
ssh:
  user: tester
  keys_only: true
"#,
            first_ip = first.ip(),
            first_port = first.port(),
            second_ip = second.ip(),
            second_port = second.port(),
            key_path = key_path.display(),
        ),
    )
    .expect("write two-server deploy.yml");
    config_path
}

fn run_jiji_server_setup(config_path: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_jiji"))
        .arg("server")
        .arg("setup")
        .arg("-c")
        .arg(config_path)
        .output()
        .expect("run jiji server setup")
}

fn run_jiji_server_setup_with_hosts(
    config_path: &std::path::Path,
    hosts: &str,
) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_jiji"))
        .arg("server")
        .arg("setup")
        .arg("-c")
        .arg(config_path)
        .arg("-H")
        .arg(hosts)
        .output()
        .expect("run jiji server setup")
}

fn run_jiji_server_setup_with_args(
    config_path: &std::path::Path,
    extra_args: &[&str],
) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_jiji"))
        .arg("server")
        .arg("setup")
        .arg("-c")
        .arg(config_path)
        .args(extra_args)
        .output()
        .expect("run jiji server setup")
}

fn run_jiji_proxy_restart(config_path: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_jiji"))
        .arg("proxy")
        .arg("restart")
        .arg("-c")
        .arg(config_path)
        .output()
        .expect("run jiji proxy restart")
}

fn run_jiji_proxy_logs(config_path: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_jiji"))
        .arg("proxy")
        .arg("logs")
        .args(args)
        .arg("-c")
        .arg(config_path)
        .output()
        .expect("run jiji proxy logs")
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_an_already_installed_engine() {
    let client_key =
        PrivateKey::random(&mut rng(), Algorithm::Ed25519).expect("generate client key");
    let mut responses = HashMap::new();
    responses.insert("which docker".to_string(), success(""));
    responses.insert(
        "docker --version".to_string(),
        success("Docker version 99.0.0, build abcdef\n"),
    );
    add_network_setup_responses(&mut responses);

    let (addr, received, _stdin) =
        spawn_test_server(client_key.public_key().clone(), responses).await;

    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = dir.path().join("id_ed25519");
    std::fs::write(
        &key_path,
        client_key
            .to_openssh(LineEnding::LF)
            .expect("encode key as openssh")
            .as_bytes(),
    )
    .expect("write key file");

    let config_path = write_config(dir.path(), addr, &key_path);
    let output = run_jiji_server_setup(&config_path);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "expected success, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("connected"), "stdout: {stdout}");
    assert!(
        stdout.contains("docker already installed (Docker version 99.0.0, build abcdef)"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("jiji-proxy configured and running"),
        "stdout: {stdout}"
    );
    assert!(
        received
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.contains("cat >> .jiji/testproject/audit.log")),
        "a successful server setup should append an audit entry"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn installs_the_jiji_agent_when_a_local_binary_is_available() {
    let client_key =
        PrivateKey::random(&mut rng(), Algorithm::Ed25519).expect("generate client key");
    let mut responses = HashMap::new();
    responses.insert("which docker".to_string(), success(""));
    responses.insert(
        "docker --version".to_string(),
        success("Docker version 99.0.0, build abcdef\n"),
    );
    add_network_setup_responses(&mut responses);

    let (addr, received, _stdin) =
        spawn_test_server(client_key.public_key().clone(), responses).await;

    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = dir.path().join("id_ed25519");
    std::fs::write(
        &key_path,
        client_key
            .to_openssh(LineEnding::LF)
            .expect("encode key as openssh")
            .as_bytes(),
    )
    .expect("write key file");
    let config_path = write_config(dir.path(), addr, &key_path);

    let binary_path = write_fake_local_agent_binary(dir.path(), env!("JIJI_AGENT_BUILD_VERSION"));

    let output = Command::new(env!("CARGO_BIN_EXE_jiji"))
        .arg("server")
        .arg("setup")
        .arg("-c")
        .arg(&config_path)
        .env("JIJI_AGENT_BINARY", &binary_path)
        .output()
        .expect("run jiji server setup");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let paths = AgentPaths::default_for_project("testproject");
    let commands = received.lock().unwrap().clone();
    assert!(
        commands.iter().any(|c| c.starts_with("install -d -m 0700 ")
            && c.contains(&paths.project_dir.display().to_string())),
        "expected agent directories to be created: {commands:?}"
    );
    assert!(
        commands.contains(&format!(
            "install -D -m 0755 /dev/stdin {}",
            paths.binary_path.display()
        )),
        "expected the agent binary to be uploaded: {commands:?}"
    );
    assert!(
        commands.contains(&format!(
            "install -D -m 0644 /dev/stdin {}",
            paths.unit_path.display()
        )),
        "expected the agent systemd unit to be written: {commands:?}"
    );
    assert!(
        commands
            .iter()
            .any(|c| c.contains(&format!("systemctl enable --now {}", paths.unit_name))),
        "expected the agent unit to be enabled and started: {commands:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_agent_binary_falls_back_to_remote_release_download() {
    let client_key =
        PrivateKey::random(&mut rng(), Algorithm::Ed25519).expect("generate client key");
    let mut responses = HashMap::new();
    responses.insert("which docker".to_string(), success(""));
    responses.insert(
        "docker --version".to_string(),
        success("Docker version 99.0.0, build abcdef\n"),
    );
    add_network_setup_responses(&mut responses);

    let (addr, received, _stdin) =
        spawn_test_server(client_key.public_key().clone(), responses).await;

    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = dir.path().join("id_ed25519");
    std::fs::write(
        &key_path,
        client_key
            .to_openssh(LineEnding::LF)
            .expect("encode key as openssh")
            .as_bytes(),
    )
    .expect("write key file");
    let config_path = write_config(dir.path(), addr, &key_path);

    // Run a copy of the compiled `jiji` binary from an isolated directory with no sibling
    // `jiji-agent`, so `find_local_agent_binary`'s current-exe fallback naturally reports "not
    // configured" instead of finding this process's own working tree's real `jiji-agent` next
    // to `jiji` in `target/debug/` (the intended production discovery path). `JIJI_AGENT_BINARY`
    // must stay unset here: pointing it at a nonexistent path is an explicit-override failure
    // (see `invalid_explicit_agent_binary_override_fails_setup`), not this "no binary
    // configured" fallback scenario. Without a local binary, setup falls back to the
    // release-download path and runs the host-side install script; the in-process test server
    // merely records the script and answers success, so nothing touches the real network.
    let isolated_bin_dir = dir.path().join("isolated-bin");
    std::fs::create_dir(&isolated_bin_dir).expect("create isolated bin dir");
    let isolated_jiji = isolated_bin_dir.join("jiji");
    std::fs::copy(env!("CARGO_BIN_EXE_jiji"), &isolated_jiji).expect("copy jiji binary");

    let output = Command::new(&isolated_jiji)
        .arg("server")
        .arg("setup")
        .arg("-c")
        .arg(&config_path)
        .env_remove("JIJI_AGENT_BINARY")
        .output()
        .expect("run jiji server setup");
    assert!(
        output.status.success(),
        "stdout: {}, stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let commands = received.lock().unwrap().clone();
    let paths = AgentPaths::default_for_project("testproject");
    assert!(
        commands.iter().any(|c| {
            c.contains("jiji-agent-linux-")
                && c.contains("sha256sum")
                && c.contains("releases/download/jiji-agent-v")
                && c.contains(&paths.binary_path.display().to_string())
        }),
        "expected the host-side release install script to be sent: {commands:?}"
    );
    assert!(
        !commands
            .iter()
            .any(|c| c.starts_with("install -D -m 0755 /dev/stdin")),
        "remote mode must install the binary from the release, not upload it: {commands:?}"
    );
    assert!(
        commands
            .iter()
            .any(|c| c.contains(&format!("systemctl enable --now {}", paths.unit_name))),
        "expected the agent unit to be enabled and started after the release install: \
         {commands:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_explicit_agent_binary_override_fails_setup() {
    let client_key =
        PrivateKey::random(&mut rng(), Algorithm::Ed25519).expect("generate client key");
    let mut responses = HashMap::new();
    responses.insert("which docker".to_string(), success(""));
    responses.insert(
        "docker --version".to_string(),
        success("Docker version 99.0.0, build abcdef\n"),
    );
    add_network_setup_responses(&mut responses);

    let (addr, received, _stdin) =
        spawn_test_server(client_key.public_key().clone(), responses).await;

    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = dir.path().join("id_ed25519");
    std::fs::write(
        &key_path,
        client_key
            .to_openssh(LineEnding::LF)
            .expect("encode key as openssh")
            .as_bytes(),
    )
    .expect("write key file");
    let config_path = write_config(dir.path(), addr, &key_path);

    // An explicit but invalid `JIJI_AGENT_BINARY` must be a hard failure, not a silent fallback
    // to the release download: the operator asked for a specific binary (a custom build, a
    // pinned version) and it isn't where they said it would be.
    let output = Command::new(env!("CARGO_BIN_EXE_jiji"))
        .arg("server")
        .arg("setup")
        .arg("-c")
        .arg(&config_path)
        .env("JIJI_AGENT_BINARY", dir.path().join("does-not-exist"))
        .output()
        .expect("run jiji server setup");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("Phase 3 requires the authoritative jiji agent"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let commands = received.lock().unwrap().clone();
    assert!(
        !commands.iter().any(|c| c.contains("/etc/jiji/agent/")),
        "no agent commands should be sent when an explicit override is invalid: {commands:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn outdated_explicit_agent_binary_override_fails_setup() {
    let client_key =
        PrivateKey::random(&mut rng(), Algorithm::Ed25519).expect("generate client key");
    let mut responses = HashMap::new();
    responses.insert("which docker".to_string(), success(""));
    responses.insert(
        "docker --version".to_string(),
        success("Docker version 99.0.0, build abcdef\n"),
    );
    add_network_setup_responses(&mut responses);

    let (addr, received, _stdin) =
        spawn_test_server(client_key.public_key().clone(), responses).await;

    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = dir.path().join("id_ed25519");
    std::fs::write(
        &key_path,
        client_key
            .to_openssh(LineEnding::LF)
            .expect("encode key as openssh")
            .as_bytes(),
    )
    .expect("write key file");
    let config_path = write_config(dir.path(), addr, &key_path);

    // Reproduces the live bug: `jiji update` replaces only the `jiji` binary, never the
    // `jiji-agent` beside it, so a real (but stale) local agent binary can end up pointed at by
    // `JIJI_AGENT_BINARY`. Before `resolve_agent_binary_source` execed it to check, this binary
    // was uploaded as-is and the run reported the host as "already current" even though it never
    // reached `AGENT_BUILD_VERSION`. An explicit override must fail loudly instead of silently
    // using a binary the operator didn't realize was outdated.
    let binary_path = write_fake_local_agent_binary(dir.path(), "0.0.1");

    let output = Command::new(env!("CARGO_BIN_EXE_jiji"))
        .arg("server")
        .arg("setup")
        .arg("-c")
        .arg(&config_path)
        .env("JIJI_AGENT_BINARY", &binary_path)
        .output()
        .expect("run jiji server setup");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("is v0.0.1") && stderr.contains("requires at least v"),
        "stderr: {stderr}"
    );

    let commands = received.lock().unwrap().clone();
    assert!(
        !commands.iter().any(|c| c.contains("/etc/jiji/agent/")),
        "no agent commands should be sent when the explicit override is outdated: {commands:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn outdated_auto_discovered_agent_binary_falls_back_to_download() {
    let client_key =
        PrivateKey::random(&mut rng(), Algorithm::Ed25519).expect("generate client key");
    let mut responses = HashMap::new();
    responses.insert("which docker".to_string(), success(""));
    responses.insert(
        "docker --version".to_string(),
        success("Docker version 99.0.0, build abcdef\n"),
    );
    add_network_setup_responses(&mut responses);

    let (addr, received, _stdin) =
        spawn_test_server(client_key.public_key().clone(), responses).await;

    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = dir.path().join("id_ed25519");
    std::fs::write(
        &key_path,
        client_key
            .to_openssh(LineEnding::LF)
            .expect("encode key as openssh")
            .as_bytes(),
    )
    .expect("write key file");
    let config_path = write_config(dir.path(), addr, &key_path);

    // Same isolated-directory technique as `missing_agent_binary_falls_back_to_remote_release_
    // download`, but this time a real, outdated `jiji-agent` sits next to `jiji` -- the sibling-
    // discovery path (no `JIJI_AGENT_BINARY` override) must fall back to downloading the correct
    // release rather than silently uploading a binary that doesn't meet `AGENT_BUILD_VERSION`,
    // since auto-discovery is only ever an optimization over the download, never a hard
    // requirement the way an explicit override is.
    let isolated_bin_dir = dir.path().join("isolated-bin");
    std::fs::create_dir(&isolated_bin_dir).expect("create isolated bin dir");
    let isolated_jiji = isolated_bin_dir.join("jiji");
    std::fs::copy(env!("CARGO_BIN_EXE_jiji"), &isolated_jiji).expect("copy jiji binary");
    write_fake_local_agent_binary(&isolated_bin_dir, "0.0.1");
    std::fs::rename(
        isolated_bin_dir.join("fake-jiji-agent"),
        isolated_bin_dir.join("jiji-agent"),
    )
    .expect("rename fake agent binary to the sibling-discovery name");

    let output = Command::new(&isolated_jiji)
        .arg("server")
        .arg("setup")
        .arg("-c")
        .arg(&config_path)
        .env_remove("JIJI_AGENT_BINARY")
        .output()
        .expect("run jiji server setup");
    assert!(
        output.status.success(),
        "stdout: {}, stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    // `Ui::warn` prints to stdout (matching `Ui::say`), not stderr.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("is v0.0.1") && stdout.contains("downloading jiji-agent v"),
        "stdout: {stdout}"
    );

    let commands = received.lock().unwrap().clone();
    assert!(
        commands
            .iter()
            .any(|c| c.contains("jiji-agent-linux-") && c.contains("sha256sum")),
        "expected the release-download install script to run instead of uploading the outdated \
         local binary: {commands:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn hosts_filter_matches_the_configured_server_name_not_just_its_host_address() {
    // `write_config` names the server `web1` with a loopback test-server address as its `host:`
    // -- `-H web1` must match the config-key name (mirroring `NetworkPlan::select_hosts`'s
    // documented name-or-host matching, used by `deploy`/`teardown`), not just `server.host`.
    let client_key =
        PrivateKey::random(&mut rng(), Algorithm::Ed25519).expect("generate client key");
    let mut responses = HashMap::new();
    responses.insert("which docker".to_string(), success(""));
    responses.insert(
        "docker --version".to_string(),
        success("Docker version 99.0.0, build abcdef\n"),
    );
    add_network_setup_responses(&mut responses);

    let (addr, _received, _stdin) =
        spawn_test_server(client_key.public_key().clone(), responses).await;

    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = dir.path().join("id_ed25519");
    std::fs::write(
        &key_path,
        client_key
            .to_openssh(LineEnding::LF)
            .expect("encode key as openssh")
            .as_bytes(),
    )
    .expect("write key file");

    let config_path = write_config(dir.path(), addr, &key_path);
    let output = run_jiji_server_setup_with_hosts(&config_path, "missing,web1");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "expected success, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("Targeting 1 server(s): web1"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("Host filter 'missing' matched no servers"),
        "stdout: {stdout}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn filtered_setup_bootstraps_from_one_seed_without_reconciling_that_seed() {
    let client_key =
        PrivateKey::random(&mut rng(), Algorithm::Ed25519).expect("generate client key");
    let mut responses = HashMap::new();
    responses.insert("which docker".to_string(), success(""));
    responses.insert(
        "docker --version".to_string(),
        success("Docker version 99.0.0, build abcdef\n"),
    );
    add_network_setup_responses(&mut responses);

    let (existing_addr, existing_received, _existing_stdin) =
        spawn_test_server(client_key.public_key().clone(), responses.clone()).await;
    let (new_addr, new_received, _new_stdin) =
        spawn_test_server(client_key.public_key().clone(), responses).await;

    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = dir.path().join("id_ed25519");
    std::fs::write(
        &key_path,
        client_key
            .to_openssh(LineEnding::LF)
            .expect("encode key as openssh")
            .as_bytes(),
    )
    .expect("write key file");
    let config_path = write_two_server_config(dir.path(), existing_addr, new_addr, &key_path);

    let output = run_jiji_server_setup_with_hosts(&config_path, "new-host");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let existing_commands = existing_received.lock().expect("received mutex poisoned");
    let new_commands = new_received.lock().expect("received mutex poisoned");

    assert!(
        !existing_commands
            .iter()
            .any(|command| command == "which docker"),
        "the host filter must still limit engine setup: {existing_commands:?}"
    );
    assert!(
        new_commands.iter().any(|command| command == "which docker"),
        "the selected host must receive engine setup: {new_commands:?}"
    );
    assert!(
        !existing_commands
            .iter()
            .any(|command| command.contains("wireguard.conf.input")),
        "the seed must not have its network generation rewritten: {existing_commands:?}"
    );
    assert!(
        existing_commands
            .iter()
            .any(|command| command.contains("public.key")),
        "the selected host must obtain the reachable seed's public key: {existing_commands:?}"
    );
    assert!(
        new_commands
            .iter()
            .any(|command| command.contains("wireguard.conf.input")),
        "a stale topology must reconcile the selected host: {new_commands:?}"
    );
    assert!(
        !existing_commands
            .iter()
            .any(|command| command == &format!("docker pull {}", jiji_network::image())),
        "the host filter must still limit proxy setup: {existing_commands:?}"
    );
    assert!(
        new_commands
            .iter()
            .any(|command| command == &format!("docker pull {}", jiji_network::image())),
        "the selected host must receive proxy setup: {new_commands:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn filtered_setup_enrolls_the_new_host_as_a_live_peer_on_the_seeds_own_interface() {
    // Confirmed live (2026-07-30): before this fix, a targeted `-H new-host` run only ever staged
    // the *new* host's own generation -- the seed's own WireGuard interface was never touched, so
    // it never dialed out to the new host first. For a cloud+home mixed topology this broke
    // connectivity outright: WireGuard can only learn a NATed peer's real, currently-routable
    // endpoint from that peer's own first packet (its built-in endpoint roaming), so a brand-new
    // server whose seed sits behind NAT with a private-LAN `host:` address could never reach that
    // seed at all until the seed reached out first.
    let client_key =
        PrivateKey::random(&mut rng(), Algorithm::Ed25519).expect("generate client key");
    let mut responses = HashMap::new();
    responses.insert("which docker".to_string(), success(""));
    responses.insert(
        "docker --version".to_string(),
        success("Docker version 99.0.0, build abcdef\n"),
    );
    add_network_setup_responses(&mut responses);

    let (existing_addr, existing_received, _existing_stdin) =
        spawn_test_server(client_key.public_key().clone(), responses.clone()).await;
    let (new_addr, _new_received, _new_stdin) =
        spawn_test_server(client_key.public_key().clone(), responses).await;

    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = dir.path().join("id_ed25519");
    std::fs::write(
        &key_path,
        client_key
            .to_openssh(LineEnding::LF)
            .expect("encode key as openssh")
            .as_bytes(),
    )
    .expect("write key file");
    let config_path = write_two_server_config(dir.path(), existing_addr, new_addr, &key_path);

    let output = run_jiji_server_setup_with_hosts(&config_path, "new-host");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let existing_commands = existing_received.lock().expect("received mutex poisoned");
    assert!(
        existing_commands.iter().any(|command| command
            .starts_with("wg set ")
            && command.contains("peer test-wireguard-public-key")
            && command.contains("endpoint 127.0.0.1:")
            && command.contains("persistent-keepalive 25")),
        "the seed should receive a live `wg set` adding the new host as a peer: {existing_commands:?}"
    );
    assert!(
        !existing_commands
            .iter()
            .any(|command| command.contains("wireguard.conf.input")),
        "enrolling the new peer must not rewrite the seed's own generation: {existing_commands:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn installs_a_missing_engine() {
    let client_key =
        PrivateKey::random(&mut rng(), Algorithm::Ed25519).expect("generate client key");
    let mut responses = HashMap::new();
    responses.insert("which docker".to_string(), failure());
    responses.insert(
        "cat /etc/os-release".to_string(),
        success("ID=ubuntu\nVERSION_ID=\"24.04\"\nVERSION_CODENAME=noble\n"),
    );
    responses.insert(
        "docker --version".to_string(),
        success("Docker version 99.0.0, build abcdef\n"),
    );
    add_network_setup_responses(&mut responses);
    // Every install command not explicitly listed defaults to success (empty CannedResponse).

    let (addr, _, _stdin) = spawn_test_server(client_key.public_key().clone(), responses).await;

    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = dir.path().join("id_ed25519");
    std::fs::write(
        &key_path,
        client_key
            .to_openssh(LineEnding::LF)
            .expect("encode key as openssh")
            .as_bytes(),
    )
    .expect("write key file");

    let config_path = write_config(dir.path(), addr, &key_path);
    let output = run_jiji_server_setup(&config_path);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "expected success, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("docker installed (Docker version 99.0.0, build abcdef)"),
        "stdout: {stdout}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn proxy_restart_forces_pull_remove_and_recreate() {
    let client_key =
        PrivateKey::random(&mut rng(), Algorithm::Ed25519).expect("generate client key");
    let mut responses = HashMap::new();
    responses.insert(
        "docker inspect jiji-proxy --format '{{.State.Status}}'".to_string(),
        success("running\n"),
    );
    let (addr, received, _stdin) =
        spawn_test_server(client_key.public_key().clone(), responses).await;

    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = dir.path().join("id_ed25519");
    std::fs::write(
        &key_path,
        client_key
            .to_openssh(LineEnding::LF)
            .expect("encode key as openssh")
            .as_bytes(),
    )
    .expect("write key file");
    let config_path = write_config(dir.path(), addr, &key_path);

    let config: jiji_config::Config =
        serde_yaml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    let plan = jiji_network::NetworkPlanner::new().plan(&config).unwrap();
    let server_plan = &plan.servers["web1"];

    let output = run_jiji_proxy_restart(&config_path);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let commands = received.lock().expect("received mutex poisoned");
    assert!(commands
        .iter()
        .any(|command| command == &format!("docker pull {}", jiji_network::image())));
    // Remove-then-run is one combined, flock-wrapped remote command (not two separate SSH
    // round-trips) so it can never race jiji-agent's own reconcile loop creating the same
    // container concurrently -- see `proxy.rs::recreate`'s own doc comment.
    assert!(
        commands
            .iter()
            .any(|command| command.contains("flock --close --timeout 60")
                && command.contains(jiji_agent::host_lease::DEFAULT_PATH)
                && command.contains("docker container rm -f jiji-proxy")
                && command.contains(&format!(
                    "docker run --name jiji-proxy --network {} --ip {} ",
                    server_plan.bridge_name, server_plan.proxy_address
                ))),
        "expected one flock-wrapped rm+run command: {commands:?}"
    );
    assert!(
        !commands
            .iter()
            .any(|command| command.contains("index .Config.Labels \"jiji.proxy-config\"")),
        "force restart must skip the fingerprint inspection"
    );
    assert!(
        commands.iter().any(|command| command
            == &format!(
                "docker network connect --ip {} {} jiji-proxy",
                server_plan.proxy_address, server_plan.bridge_name
            )),
        "jiji-proxy should be attached to this project's bridge after recreation: {commands:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn proxy_logs_sends_quoted_filters_and_prints_host_output() {
    let client_key =
        PrivateKey::random(&mut rng(), Algorithm::Ed25519).expect("generate client key");
    let command =
        "docker logs --timestamps --since='1 hour ago' jiji-proxy | grep -- 'can'\\''t; echo bad'";
    let mut responses = HashMap::new();
    responses.insert(command.to_string(), success("matched proxy line\n"));
    let (addr, received, _stdin) =
        spawn_test_server(client_key.public_key().clone(), responses).await;

    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = dir.path().join("id_ed25519");
    std::fs::write(
        &key_path,
        client_key
            .to_openssh(LineEnding::LF)
            .expect("encode key as openssh")
            .as_bytes(),
    )
    .expect("write key file");
    let config_path = write_config(dir.path(), addr, &key_path);

    let output = run_jiji_proxy_logs(
        &config_path,
        &["--since", "1 hour ago", "--grep", "can't; echo bad"],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("web1:"), "stdout: {stdout}");
    assert!(stdout.contains("matched proxy line"), "stdout: {stdout}");
    assert!(received
        .lock()
        .expect("received mutex poisoned")
        .iter()
        .any(|received| received == command));
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_a_connection_failure_without_touching_the_engine() {
    // No server at all -- connect should fail and the command should exit non-zero with an
    // actionable message, rather than panicking or hanging.
    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = dir.path().join("id_ed25519");
    let client_key =
        PrivateKey::random(&mut rng(), Algorithm::Ed25519).expect("generate client key");
    std::fs::write(
        &key_path,
        client_key
            .to_openssh(LineEnding::LF)
            .expect("encode key as openssh")
            .as_bytes(),
    )
    .expect("write key file");

    // Bind and immediately drop, to get a port nothing is listening on.
    let addr = {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind throwaway listener");
        listener.local_addr().expect("read listener addr")
    };

    let config_path = write_config(dir.path(), addr, &key_path);
    let output = Command::new(env!("CARGO_BIN_EXE_jiji"))
        .arg("server")
        .arg("setup")
        .arg("-c")
        .arg(&config_path)
        .env("JIJI_TEST_SSH_REFUSAL_COOLDOWN_MS", "1")
        .output()
        .expect("run jiji server setup");

    assert!(!output.status.success(), "expected a non-zero exit code");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Server setup failed"), "stderr: {stderr}");
}

/// Builds a `MembershipRecord` as if it were already known from a previous `server setup` run,
/// so a test can simulate "this host already has membership history" without a real two-pass
/// setup. Only the fields `reconcile_record` actually compares (`wireguard_public_key`,
/// `endpoints`, `state`) plus `owner_epoch`/`revision` matter for these tests.
fn seeded_record(
    server_name: &str,
    wireguard_public_key: &str,
    endpoint: &str,
    owner_epoch: u64,
    revision: u64,
) -> jiji_agent::membership::MembershipRecord {
    jiji_agent::membership::MembershipRecord {
        project_id: "testproject".into(),
        recovery_epoch: 1,
        protocol_version: jiji_agent::membership::MEMBERSHIP_PROTOCOL_VERSION,
        schema_version: jiji_agent::membership::MEMBERSHIP_SCHEMA_VERSION,
        node_id: server_name.into(),
        server_name: server_name.into(),
        wireguard_public_key: wireguard_public_key.into(),
        management_address: std::net::Ipv4Addr::new(100, 64, 0, 1),
        container_subnet: "198.18.1.0/24".into(),
        endpoints: vec![endpoint.parse().expect("valid seeded endpoint")],
        owner_epoch,
        revision,
        state: jiji_agent::membership::MembershipState::Active,
    }
}

fn membership_export_command(project: &str) -> String {
    let paths = AgentPaths::default_for_project(project);
    format!(
        "{} membership-export --state-dir {}",
        paths.binary_path.display(),
        paths.state_dir.display()
    )
}

fn membership_update_stdin_command(project: &str) -> String {
    let paths = AgentPaths::default_for_project(project);
    format!(
        "install -D -m 0600 /dev/stdin {}",
        paths.project_dir.join("membership-update.json").display()
    )
}

/// Finds the last captured stdin payload for `command` and parses it as the JSON body
/// `push_membership` sends (`Vec<MembershipRecord>`).
fn last_pushed_records(
    stdin: &ReceivedStdin,
    command: &str,
) -> Vec<jiji_agent::membership::MembershipRecord> {
    let captured = stdin.lock().expect("received_stdin mutex poisoned");
    let (_, bytes) = captured
        .iter()
        .rev()
        .find(|(recorded_command, _)| recorded_command == command)
        .expect("expected a captured membership-update push");
    serde_json::from_slice(bytes).expect("membership push payload must be valid JSON")
}

#[tokio::test(flavor = "multi_thread")]
async fn endpoint_drift_is_reconciled_without_a_dedicated_command() {
    // Simulates "the server's `host:` changed since the last `server setup` run" -- there is no
    // `update-endpoint` command anymore; a plain re-run of `server setup` must pick this up on
    // its own by comparing the freshly observed endpoint against the last known membership record.
    let client_key =
        PrivateKey::random(&mut rng(), Algorithm::Ed25519).expect("generate client key");
    let mut responses = HashMap::new();
    responses.insert("which docker".to_string(), success(""));
    responses.insert(
        "docker --version".to_string(),
        success("Docker version 99.0.0, build abcdef\n"),
    );
    add_network_setup_responses(&mut responses);
    let seed = vec![seeded_record(
        "web1",
        "test-wireguard-public-key",
        "203.0.113.9:9999",
        1,
        4,
    )];
    responses.insert(
        membership_export_command("testproject"),
        success(&serde_json::to_string(&seed).expect("serialize seeded record")),
    );

    let (addr, _received, stdin) =
        spawn_test_server(client_key.public_key().clone(), responses).await;

    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = dir.path().join("id_ed25519");
    std::fs::write(
        &key_path,
        client_key
            .to_openssh(LineEnding::LF)
            .expect("encode key as openssh")
            .as_bytes(),
    )
    .expect("write key file");
    let config_path = write_config(dir.path(), addr, &key_path);

    let output = run_jiji_server_setup(&config_path);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let pushed = last_pushed_records(&stdin, &membership_update_stdin_command("testproject"));
    let web1 = pushed
        .iter()
        .find(|record| record.server_name == "web1")
        .expect("web1's record must be pushed");
    assert_eq!(
        web1.owner_epoch, 1,
        "an endpoint-only change must never fence a new owner_epoch"
    );
    assert_eq!(web1.revision, 5, "the seeded revision 4 must be bumped");
    assert_ne!(
        web1.endpoints,
        vec!["203.0.113.9:9999".parse().unwrap()],
        "the stale seeded endpoint must be replaced with the freshly observed one"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn removing_a_server_from_config_tombstones_it_with_yes() {
    // There is no `decommission` command anymore: removing a server from `servers:` and
    // re-running `server setup -y` on the survivors must tombstone it on its own.
    let client_key =
        PrivateKey::random(&mut rng(), Algorithm::Ed25519).expect("generate client key");
    let mut responses = HashMap::new();
    responses.insert("which docker".to_string(), success(""));
    responses.insert(
        "docker --version".to_string(),
        success("Docker version 99.0.0, build abcdef\n"),
    );
    add_network_setup_responses(&mut responses);
    // web1 (still in config) already knows about web2, which has since been removed from
    // `servers:` -- this is exactly what a surviving peer's own local store would still hold.
    let seed = vec![seeded_record(
        "web2",
        "web2-public-key",
        "203.0.113.10:51820",
        1,
        2,
    )];
    responses.insert(
        membership_export_command("testproject"),
        success(&serde_json::to_string(&seed).expect("serialize seeded record")),
    );

    let (addr, _received, stdin) =
        spawn_test_server(client_key.public_key().clone(), responses).await;

    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = dir.path().join("id_ed25519");
    std::fs::write(
        &key_path,
        client_key
            .to_openssh(LineEnding::LF)
            .expect("encode key as openssh")
            .as_bytes(),
    )
    .expect("write key file");
    let config_path = write_config(dir.path(), addr, &key_path);

    let output = run_jiji_server_setup_with_args(&config_path, &["-y"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let pushed = last_pushed_records(&stdin, &membership_update_stdin_command("testproject"));
    let web2 = pushed
        .iter()
        .find(|record| record.server_name == "web2")
        .expect("web2's tombstone must be pushed");
    assert_eq!(
        web2.state,
        jiji_agent::membership::MembershipState::Tombstoned
    );
    assert_eq!(web2.revision, 3, "the seeded revision 2 must be bumped");
    assert_eq!(
        web2.owner_epoch, 1,
        "decommission never fences a new owner_epoch"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rotate_key_forces_regeneration_and_fences_the_old_owner() {
    let client_key =
        PrivateKey::random(&mut rng(), Algorithm::Ed25519).expect("generate client key");
    let mut responses = HashMap::new();
    responses.insert("which docker".to_string(), success(""));
    responses.insert(
        "docker --version".to_string(),
        success("Docker version 99.0.0, build abcdef\n"),
    );
    add_network_setup_responses(&mut responses);
    // A different key than `add_network_setup_responses`'s canned "test-wireguard-public-key" --
    // simulates a host whose key is about to be force-rotated.
    let seed = vec![seeded_record(
        "web1",
        "old-test-wireguard-public-key",
        "127.0.0.1:1",
        1,
        4,
    )];
    responses.insert(
        membership_export_command("testproject"),
        success(&serde_json::to_string(&seed).expect("serialize seeded record")),
    );

    let (addr, received, stdin) =
        spawn_test_server(client_key.public_key().clone(), responses).await;

    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = dir.path().join("id_ed25519");
    std::fs::write(
        &key_path,
        client_key
            .to_openssh(LineEnding::LF)
            .expect("encode key as openssh")
            .as_bytes(),
    )
    .expect("write key file");
    let config_path = write_config(dir.path(), addr, &key_path);

    let output = run_jiji_server_setup_with_args(&config_path, &["-y", "--rotate-key"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let commands = received.lock().expect("received mutex poisoned");
    assert!(
        commands
            .iter()
            .any(|command| command.contains("wg genkey") && command.contains("rm -f")),
        "expected a forced keypair regeneration command: {commands:?}"
    );
    drop(commands);

    // The seeded owner_epoch=1 record for "web1" is never separately transmitted as a tombstone:
    // a strictly higher owner_epoch alone is what every peer's own CRDT needs to fence the old
    // identity out (see `reconcile_record`'s doc comment), so only the new, fenced record ships.
    let pushed = last_pushed_records(&stdin, &membership_update_stdin_command("testproject"));
    let fenced = pushed
        .iter()
        .find(|record| record.server_name == "web1")
        .expect("web1's fenced record must be pushed");
    assert_eq!(
        fenced.state,
        jiji_agent::membership::MembershipState::Active
    );
    assert_eq!(
        fenced.owner_epoch, 2,
        "a key change must fence a new owner_epoch"
    );
    assert_eq!(fenced.revision, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn rotate_key_without_yes_or_a_terminal_bails_before_touching_any_host() {
    let client_key =
        PrivateKey::random(&mut rng(), Algorithm::Ed25519).expect("generate client key");
    let mut responses = HashMap::new();
    responses.insert("which docker".to_string(), success(""));
    responses.insert(
        "docker --version".to_string(),
        success("Docker version 99.0.0, build abcdef\n"),
    );
    add_network_setup_responses(&mut responses);

    let (addr, received, _stdin) =
        spawn_test_server(client_key.public_key().clone(), responses).await;

    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = dir.path().join("id_ed25519");
    std::fs::write(
        &key_path,
        client_key
            .to_openssh(LineEnding::LF)
            .expect("encode key as openssh")
            .as_bytes(),
    )
    .expect("write key file");
    let config_path = write_config(dir.path(), addr, &key_path);

    let output = run_jiji_server_setup_with_args(&config_path, &["--rotate-key"]);
    assert!(!output.status.success(), "expected a non-zero exit code");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Refusing to prompt for confirmation"),
        "stderr: {stderr}"
    );
    assert!(
        received.lock().expect("received mutex poisoned").is_empty(),
        "the command must bail before connecting to any host"
    );
}

fn container_list_command(project: &str) -> String {
    format!(
        "docker ps -a --filter label=jiji.managed=true --filter label=jiji.project={project} \
         --format '{{{{.Names}}}}|{{{{.Label \"jiji.project\"}}}}|{{{{.Label \"jiji.service\"}}}}|{{{{.Label \"jiji.server\"}}}}|{{{{.State}}}}'"
    )
}

fn agent_request_command(paths: &AgentPaths, kind: &str) -> String {
    format!(
        "{} request --socket {} # jiji-request:{kind}",
        paths.binary_path.display(),
        paths.socket_path.display()
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn import_reports_and_commits_a_replica_with_no_existing_catalog_record() {
    // There is no standalone `jiji network assess`/`jiji network import` anymore: `--import` on
    // `server setup` must report the same "importable" finding and actually commit it once the
    // agent it just installed is up.
    let client_key =
        PrivateKey::random(&mut rng(), Algorithm::Ed25519).expect("generate client key");
    let mut responses = HashMap::new();
    responses.insert("which docker".to_string(), success(""));
    responses.insert(
        "docker --version".to_string(),
        success("Docker version 99.0.0, build abcdef\n"),
    );
    add_network_setup_responses(&mut responses);
    let paths = AgentPaths::default_for_project("testproject");
    responses.insert(
        container_list_command("testproject"),
        success("testproject-web-a|testproject|web|web1|running\n"),
    );
    responses.insert(
        agent_request_command(&paths, "catalog-list"),
        success(r#"{"Ok":{"type":"catalog_list","records":[]}}"#),
    );
    responses.insert(
        "docker inspect testproject-web-a --format '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' 2>/dev/null || true".to_string(),
        success("100.64.0.10\n"),
    );
    responses.insert(
        "docker inspect testproject-web-a --format '{{.Config.Image}}' 2>/dev/null || true"
            .to_string(),
        success("nginx:alpine\n"),
    );
    responses.insert(
        agent_request_command(&paths, "catalog-commit"),
        success(
            r#"{"Ok":{"type":"catalog_committed","record":{"project_id":"testproject","recovery_epoch":1,"protocol_version":1,"schema_version":2,"service":"web","replica_id":"web-test","owner_node_id":"web1","owner_epoch":1,"revision":1,"deployment_id":"imported-testproject-web-a","address":"100.64.0.10","ports":[],"image":"nginx:alpine","state":"stopped","health":"unknown"}}}"#,
        ),
    );

    let (addr, received, _stdin) =
        spawn_test_server(client_key.public_key().clone(), responses).await;

    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = dir.path().join("id_ed25519");
    std::fs::write(
        &key_path,
        client_key
            .to_openssh(LineEnding::LF)
            .expect("encode key as openssh")
            .as_bytes(),
    )
    .expect("write key file");
    let config_path = write_config_with_web_service(dir.path(), addr, &key_path);

    let output = run_jiji_server_setup_with_args(&config_path, &["-y", "--import"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stdout: {stdout} stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("importable -- web"),
        "expected an importable finding for web, stdout: {stdout}"
    );
    assert!(
        stdout.contains("web -> replica"),
        "expected the import plan to list the web replica, stdout: {stdout}"
    );
    assert!(
        received
            .lock()
            .expect("received mutex poisoned")
            .iter()
            .any(|command| command.contains("# jiji-request:catalog-commit")),
        "expected the import to actually commit a catalog record"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn import_dry_run_reports_the_plan_without_committing_anything() {
    let client_key =
        PrivateKey::random(&mut rng(), Algorithm::Ed25519).expect("generate client key");
    let mut responses = HashMap::new();
    responses.insert("which docker".to_string(), success(""));
    responses.insert(
        "docker --version".to_string(),
        success("Docker version 99.0.0, build abcdef\n"),
    );
    add_network_setup_responses(&mut responses);
    let paths = AgentPaths::default_for_project("testproject");
    responses.insert(
        container_list_command("testproject"),
        success("testproject-web-a|testproject|web|web1|running\n"),
    );
    responses.insert(
        agent_request_command(&paths, "catalog-list"),
        success(r#"{"Ok":{"type":"catalog_list","records":[]}}"#),
    );
    responses.insert(
        "docker inspect testproject-web-a --format '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' 2>/dev/null || true".to_string(),
        success("100.64.0.10\n"),
    );
    responses.insert(
        "docker inspect testproject-web-a --format '{{.Config.Image}}' 2>/dev/null || true"
            .to_string(),
        success("nginx:alpine\n"),
    );

    let (addr, received, _stdin) =
        spawn_test_server(client_key.public_key().clone(), responses).await;

    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = dir.path().join("id_ed25519");
    std::fs::write(
        &key_path,
        client_key
            .to_openssh(LineEnding::LF)
            .expect("encode key as openssh")
            .as_bytes(),
    )
    .expect("write key file");
    let config_path = write_config_with_web_service(dir.path(), addr, &key_path);

    let output =
        run_jiji_server_setup_with_args(&config_path, &["-y", "--import", "--import-dry-run"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stdout: {stdout} stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("web -> replica"),
        "expected the dry-run plan to list the web replica, stdout: {stdout}"
    );
    assert!(
        !received
            .lock()
            .expect("received mutex poisoned")
            .iter()
            .any(|command| command.contains("# jiji-request:catalog-commit")),
        "dry-run must never commit anything"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn import_reports_nothing_to_import_when_a_live_catalog_record_already_exists() {
    let client_key =
        PrivateKey::random(&mut rng(), Algorithm::Ed25519).expect("generate client key");
    let mut responses = HashMap::new();
    responses.insert("which docker".to_string(), success(""));
    responses.insert(
        "docker --version".to_string(),
        success("Docker version 99.0.0, build abcdef\n"),
    );
    add_network_setup_responses(&mut responses);
    let paths = AgentPaths::default_for_project("testproject");
    let replica_id = jiji_cli::placement::replica_id("testproject", "web", 0);
    responses.insert(
        container_list_command("testproject"),
        success("testproject-web-a|testproject|web|web1|running\n"),
    );
    responses.insert(
        agent_request_command(&paths, "catalog-list"),
        success(&format!(
            r#"{{"Ok":{{"type":"catalog_list","records":[{{"project_id":"testproject","recovery_epoch":1,"protocol_version":1,"schema_version":2,"service":"web","replica_id":"{replica_id}","owner_node_id":"web1","owner_epoch":1,"revision":3,"deployment_id":"deploy-live","address":"100.64.0.20","ports":[],"image":"nginx:alpine","state":"active","health":"healthy"}}]}}}}"#
        )),
    );

    let (addr, received, _stdin) =
        spawn_test_server(client_key.public_key().clone(), responses).await;

    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = dir.path().join("id_ed25519");
    std::fs::write(
        &key_path,
        client_key
            .to_openssh(LineEnding::LF)
            .expect("encode key as openssh")
            .as_bytes(),
    )
    .expect("write key file");
    let config_path = write_config_with_web_service(dir.path(), addr, &key_path);

    let output = run_jiji_server_setup_with_args(&config_path, &["-y", "--import"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stdout: {stdout} stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("Nothing to import"),
        "an already-live replica must never be re-imported: {stdout}"
    );
    assert!(
        !received
            .lock()
            .expect("received mutex poisoned")
            .iter()
            .any(|command| command.contains("# jiji-request:catalog-commit")),
        "a live replica must never be committed over"
    );
}
