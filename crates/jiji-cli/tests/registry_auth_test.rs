//! Integration tests for `jiji registry login`/`jiji registry logout`, run as real subprocesses.
//! Remote scope is exercised against a real, in-process SSH server (mirroring `deploy_test.rs`'s
//! pattern); local scope is exercised against a fake `docker`/`podman` executable on PATH that
//! records argv and stdin separately (mirroring `registry_teardown_test.rs`'s pattern).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rand::rng;
use russh::keys::ssh_key::LineEnding;
use russh::keys::{Algorithm, PrivateKey, PublicKey};
use russh::server::{self, Auth, ChannelOpenHandle, Server as _, Session};
use russh::{Channel, ChannelId};
use tokio::net::TcpListener;

#[derive(Clone, Default)]
struct CannedResponse {
    success: bool,
    stdout: String,
    stderr: String,
}

fn success(stdout: &str) -> CannedResponse {
    CannedResponse {
        success: true,
        stdout: stdout.to_string(),
        stderr: String::new(),
    }
}

fn failure(stderr: &str) -> CannedResponse {
    CannedResponse {
        success: false,
        stdout: String::new(),
        stderr: stderr.to_string(),
    }
}

#[derive(Clone)]
struct TestServer {
    authorized_key: PublicKey,
    responses: Arc<HashMap<String, CannedResponse>>,
    pending: Arc<Mutex<HashMap<ChannelId, String>>>,
    stdin: Arc<Mutex<HashMap<ChannelId, Vec<u8>>>>,
    received: Arc<Mutex<Vec<String>>>,
    received_stdin: Arc<Mutex<Vec<u8>>>,
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
                .extend_from_slice(&stdin);
        }

        let response = self
            .responses
            .get(&command)
            .cloned()
            .unwrap_or_else(|| success(""));

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

struct Harness {
    received: Arc<Mutex<Vec<String>>>,
    received_stdin: Arc<Mutex<Vec<u8>>>,
}

async fn spawn_test_server(
    authorized_key: PublicKey,
    responses: HashMap<String, CannedResponse>,
) -> (Harness, SocketAddr) {
    spawn_test_server_on("127.0.0.1", authorized_key, responses).await
}

/// Like `spawn_test_server`, but binds a caller-chosen loopback address (all of `127.0.0.0/8` is
/// loopback), so two servers in the same test can be distinguished by `-H`/`--hosts`, which
/// matches the configured host address rather than the server's config-file name.
async fn spawn_test_server_on(
    bind_ip: &str,
    authorized_key: PublicKey,
    responses: HashMap<String, CannedResponse>,
) -> (Harness, SocketAddr) {
    let config = Arc::new(server::Config {
        auth_rejection_time: Duration::from_millis(50),
        keys: vec![PrivateKey::random(&mut rng(), Algorithm::Ed25519).expect("generate host key")],
        ..Default::default()
    });

    let listener = TcpListener::bind(format!("{bind_ip}:0"))
        .await
        .expect("bind test listener");
    let addr = listener.local_addr().expect("read listener addr");

    let received = Arc::new(Mutex::new(Vec::new()));
    let received_stdin = Arc::new(Mutex::new(Vec::new()));
    let mut test_server = TestServer {
        authorized_key,
        responses: Arc::new(responses),
        pending: Arc::new(Mutex::new(HashMap::new())),
        stdin: Arc::new(Mutex::new(HashMap::new())),
        received: received.clone(),
        received_stdin: received_stdin.clone(),
    };

    tokio::spawn(async move {
        let _ = test_server.run_on_socket(config, &listener).await;
        drop(listener);
    });

    (
        Harness {
            received,
            received_stdin,
        },
        addr,
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn login_command(engine: &str, server: &str, username: &str) -> String {
    format!(
        "{engine} login {} --username {} --password-stdin",
        shell_quote(server),
        shell_quote(username)
    )
}

fn logout_command(engine: &str, server: &str) -> String {
    format!("{engine} logout {}", shell_quote(server))
}

/// One remote server ("app") with a remote registry configured with a literal password.
fn config_yaml(addr: SocketAddr, key_path: &std::path::Path, engine: &str) -> String {
    format!(
        r#"
project: demo
builder:
  engine: {engine}
  registry:
    type: remote
    server: registry.example.com
    username: alice
    password: s3cret
servers:
  app:
    host: {ip}
    port: {port}
    keys:
      - {key_path}
services:
  web:
    image: example/web:latest
    hosts: [app]
ssh:
  user: tester
  keys_only: true
  connect_timeout: 2
"#,
        engine = engine,
        ip = addr.ip(),
        port = addr.port(),
        key_path = key_path.display(),
    )
}

/// Two remote servers ("app1", "app2"), otherwise identical to `config_yaml`.
fn config_yaml_two_servers(
    addr1: SocketAddr,
    addr2: SocketAddr,
    key_path: &std::path::Path,
    engine: &str,
) -> String {
    format!(
        r#"
project: demo
builder:
  engine: {engine}
  registry:
    type: remote
    server: registry.example.com
    username: alice
    password: s3cret
servers:
  app1:
    host: {ip1}
    port: {port1}
    keys:
      - {key_path}
  app2:
    host: {ip2}
    port: {port2}
    keys:
      - {key_path}
services:
  web:
    image: example/web:latest
    hosts: [app1]
ssh:
  user: tester
  keys_only: true
  connect_timeout: 2
"#,
        ip1 = addr1.ip(),
        port1 = addr1.port(),
        ip2 = addr2.ip(),
        port2 = addr2.port(),
        key_path = key_path.display(),
    )
}

/// No `ssh:` section at all, to prove `--skip-remote` never requires one.
fn config_yaml_no_ssh(engine: &str) -> String {
    format!(
        r#"
project: demo
builder:
  engine: {engine}
  registry:
    type: remote
    server: registry.example.com
    username: alice
    password: s3cret
servers:
  app:
    host: 198.51.100.5
services:
  web:
    image: example/web:latest
    hosts: [app]
"#,
    )
}

fn config_yaml_local_registry(addr: SocketAddr, key_path: &std::path::Path) -> String {
    format!(
        r#"
project: demo
builder:
  engine: docker
  registry:
    type: local
    port: 31270
servers:
  app:
    host: {ip}
    port: {port}
    keys:
      - {key_path}
services:
  web:
    image: example/web:latest
    hosts: [app]
ssh:
  user: tester
  keys_only: true
"#,
        ip = addr.ip(),
        port = addr.port(),
        key_path = key_path.display(),
    )
}

fn write_config_str(dir: &std::path::Path, contents: &str) -> std::path::PathBuf {
    let config_path = dir.join("deploy.yml");
    std::fs::write(&config_path, contents).expect("write test deploy.yml");
    config_path
}

fn setup_test_dir() -> (tempfile::TempDir, std::path::PathBuf, PrivateKey) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let client_key =
        PrivateKey::random(&mut rng(), Algorithm::Ed25519).expect("generate client key");
    let key_path = dir.path().join("id_ed25519");
    std::fs::write(
        &key_path,
        client_key
            .to_openssh(LineEnding::LF)
            .expect("encode key as openssh")
            .as_bytes(),
    )
    .expect("write key file");
    (dir, key_path, client_key)
}

/// Writes a fake `docker`/`podman` executable to `<dir>/bin/<name>` that appends its argv to
/// `$JIJI_TEST_LOG`, drains stdin to `$JIJI_TEST_STDIN` when the first argument is `login`, and
/// exits according to `$JIJI_TEST_LOGIN_EXIT`/`$JIJI_TEST_LOGOUT_EXIT` (and their matching
/// `_STDERR` variables) when the first argument matches.
fn write_fake_engine(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).expect("create bin");
    let path = bin.join(name);
    std::fs::write(
        &path,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$JIJI_TEST_LOG"
if [ "$1" = "login" ]; then
  if [ -n "$JIJI_TEST_STDIN" ]; then
    cat > "$JIJI_TEST_STDIN"
  else
    cat > /dev/null
  fi
  if [ -n "$JIJI_TEST_LOGIN_STDERR" ]; then
    printf '%s\n' "$JIJI_TEST_LOGIN_STDERR" 1>&2
  fi
  exit "${JIJI_TEST_LOGIN_EXIT:-0}"
fi
if [ "$1" = "logout" ]; then
  if [ -n "$JIJI_TEST_LOGOUT_STDERR" ]; then
    printf '%s\n' "$JIJI_TEST_LOGOUT_STDERR" 1>&2
  fi
  exit "${JIJI_TEST_LOGOUT_EXIT:-0}"
fi
exit 0
"#,
    )
    .expect("write fake engine");
    let mut permissions = std::fs::metadata(&path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).expect("chmod");
    bin
}

#[allow(clippy::too_many_arguments)]
fn run_jiji(
    subcommand: &str,
    config_path: &std::path::Path,
    bin: &std::path::Path,
    extra_args: &[&str],
    envs: &[(&str, &str)],
) -> std::process::Output {
    let current_path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![bin.to_path_buf()];
    paths.extend(std::env::split_paths(&current_path));

    let mut command = Command::new(env!("CARGO_BIN_EXE_jiji"));
    command
        .arg("registry")
        .arg(subcommand)
        .arg("-c")
        .arg(config_path)
        .env("PATH", std::env::join_paths(paths).expect("join PATH"));
    for arg in extra_args {
        command.arg(arg);
    }
    for (key, value) in envs {
        command.env(key, value);
    }
    command.output().expect("run jiji registry command")
}

// ---------------------------------------------------------------------------------------------
// Login
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn default_login_authenticates_locally_and_on_every_configured_server() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert(
        login_command("docker", "registry.example.com", "alice"),
        success(""),
    );
    let (harness, addr) = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config_str(dir.path(), &config_yaml(addr, &key_path, "docker"));
    let bin = write_fake_engine(dir.path(), "docker");
    let log = dir.path().join("commands.log");
    let stdin_capture = dir.path().join("stdin.bin");

    let output = run_jiji(
        "login",
        &config_path,
        &bin,
        &[],
        &[
            ("JIJI_TEST_LOG", log.to_str().unwrap()),
            ("JIJI_TEST_STDIN", stdin_capture.to_str().unwrap()),
        ],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stdout: {stdout} stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("completed on 2 target(s)"),
        "stdout: {stdout}"
    );

    let local_log = std::fs::read_to_string(&log).expect("read local log");
    assert!(local_log.contains("login registry.example.com --username alice --password-stdin"));
    let local_stdin = std::fs::read_to_string(&stdin_capture).expect("read local stdin");
    assert_eq!(local_stdin, "s3cret");

    let received = harness.received.lock().unwrap().clone();
    assert!(received.contains(&login_command("docker", "registry.example.com", "alice")));
    assert!(
        received.iter().all(|c| !c.contains("s3cret")),
        "password must never appear in a remote command string: {received:?}"
    );
    let received_stdin = harness.received_stdin.lock().unwrap().clone();
    assert_eq!(
        String::from_utf8_lossy(&received_stdin),
        "s3cret",
        "password must be delivered through the SSH stdin channel"
    );

    assert!(
        !stdout.contains("s3cret") && !String::from_utf8_lossy(&output.stderr).contains("s3cret"),
        "password must never be printed"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn skip_local_performs_only_remote_login() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert(
        login_command("docker", "registry.example.com", "alice"),
        success(""),
    );
    let (harness, addr) = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config_str(dir.path(), &config_yaml(addr, &key_path, "docker"));
    let bin = write_fake_engine(dir.path(), "docker");
    let log = dir.path().join("commands.log");

    let output = run_jiji(
        "login",
        &config_path,
        &bin,
        &["--skip-local"],
        &[("JIJI_TEST_LOG", log.to_str().unwrap())],
    );
    assert!(output.status.success());
    assert!(
        !log.exists() || std::fs::read_to_string(&log).unwrap().is_empty(),
        "local engine must not run when --skip-local is set"
    );
    let received = harness.received.lock().unwrap().clone();
    assert!(received.contains(&login_command("docker", "registry.example.com", "alice")));
}

#[tokio::test(flavor = "multi_thread")]
async fn skip_remote_performs_only_local_login_and_needs_no_ssh_section() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = write_config_str(dir.path(), &config_yaml_no_ssh("docker"));
    let bin = write_fake_engine(dir.path(), "docker");
    let log = dir.path().join("commands.log");
    let stdin_capture = dir.path().join("stdin.bin");

    let output = run_jiji(
        "login",
        &config_path,
        &bin,
        &["--skip-remote"],
        &[
            ("JIJI_TEST_LOG", log.to_str().unwrap()),
            ("JIJI_TEST_STDIN", stdin_capture.to_str().unwrap()),
        ],
    );
    assert!(
        output.status.success(),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let local_log = std::fs::read_to_string(&log).expect("read local log");
    assert!(local_log.contains("login registry.example.com --username alice --password-stdin"));
}

#[tokio::test(flavor = "multi_thread")]
async fn host_filter_selects_only_the_matching_server() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert(
        login_command("docker", "registry.example.com", "alice"),
        success(""),
    );
    let (harness, addr1) = spawn_test_server_on(
        "127.0.0.1",
        client_key.public_key().clone(),
        responses.clone(),
    )
    .await;
    let (_harness2, addr2) =
        spawn_test_server_on("127.0.0.2", client_key.public_key().clone(), responses).await;
    let config_path = write_config_str(
        dir.path(),
        &config_yaml_two_servers(addr1, addr2, &key_path, "docker"),
    );
    let bin = write_fake_engine(dir.path(), "docker");
    let log = dir.path().join("commands.log");

    let output = run_jiji(
        "login",
        &config_path,
        &bin,
        &["--skip-local", "-H", &addr1.ip().to_string()],
        &[("JIJI_TEST_LOG", log.to_str().unwrap())],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "stdout: {stdout}");
    assert!(stdout.contains("app1"));
    assert!(!stdout.contains("app2"));
    let received = harness.received.lock().unwrap().clone();
    assert!(received.contains(&login_command("docker", "registry.example.com", "alice")));
}

#[tokio::test(flavor = "multi_thread")]
async fn unmatched_host_filter_fails_before_any_local_login() {
    let (dir, key_path, client_key) = setup_test_dir();
    let (_harness, addr) = spawn_test_server(client_key.public_key().clone(), HashMap::new()).await;
    let config_path = write_config_str(dir.path(), &config_yaml(addr, &key_path, "docker"));
    let bin = write_fake_engine(dir.path(), "docker");
    let log = dir.path().join("commands.log");

    let output = run_jiji(
        "login",
        &config_path,
        &bin,
        &["-H", "does-not-exist"],
        &[("JIJI_TEST_LOG", log.to_str().unwrap())],
    );
    assert!(!output.status.success());
    assert!(
        !log.exists(),
        "local login must not run before an unmatched host filter is reported"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn password_resolves_from_env_file() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert(
        login_command("docker", "registry.example.com", "alice"),
        success(""),
    );
    let (_harness, addr) = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config = format!(
        r#"
project: demo
builder:
  engine: docker
  registry:
    type: remote
    server: registry.example.com
    username: alice
    password: REGISTRY_TOKEN
servers:
  app:
    host: {ip}
    port: {port}
    keys:
      - {key_path}
services:
  web:
    image: example/web:latest
    hosts: [app]
ssh:
  user: tester
  keys_only: true
"#,
        ip = addr.ip(),
        port = addr.port(),
        key_path = key_path.display(),
    );
    // `project_root_from_config_path` assumes `<project_root>/.jiji/<file>.yml`, so the `.env`
    // file must live one level above the config file to be discovered.
    let jiji_dir = dir.path().join(".jiji");
    std::fs::create_dir_all(&jiji_dir).expect("create .jiji dir");
    let config_path = jiji_dir.join("deploy.yml");
    std::fs::write(&config_path, &config).expect("write test deploy.yml");
    std::fs::write(dir.path().join(".env"), "REGISTRY_TOKEN=from-env-file\n").expect("write .env");
    let bin = write_fake_engine(dir.path(), "docker");
    let log = dir.path().join("commands.log");
    let stdin_capture = dir.path().join("stdin.bin");

    let output = run_jiji(
        "login",
        &config_path,
        &bin,
        &["--skip-remote"],
        &[
            ("JIJI_TEST_LOG", log.to_str().unwrap()),
            ("JIJI_TEST_STDIN", stdin_capture.to_str().unwrap()),
        ],
    );
    assert!(
        output.status.success(),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdin = std::fs::read_to_string(&stdin_capture).expect("read stdin capture");
    assert_eq!(stdin, "from-env-file");
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_secret_requires_host_env_flag() {
    let (dir, key_path, client_key) = setup_test_dir();
    let (_harness, addr) = spawn_test_server(client_key.public_key().clone(), HashMap::new()).await;
    let config = format!(
        r#"
project: demo
builder:
  engine: docker
  registry:
    type: remote
    server: registry.example.com
    username: alice
    password: REGISTRY_TOKEN
servers:
  app:
    host: {ip}
    port: {port}
    keys:
      - {key_path}
services:
  web:
    image: example/web:latest
    hosts: [app]
ssh:
  user: tester
  keys_only: true
"#,
        ip = addr.ip(),
        port = addr.port(),
        key_path = key_path.display(),
    );
    let config_path = write_config_str(dir.path(), &config);
    let bin = write_fake_engine(dir.path(), "docker");
    let log = dir.path().join("commands.log");

    let without_host_env = run_jiji(
        "login",
        &config_path,
        &bin,
        &["--skip-remote"],
        &[
            ("JIJI_TEST_LOG", log.to_str().unwrap()),
            ("REGISTRY_TOKEN", "from-host-env"),
        ],
    );
    assert!(!without_host_env.status.success());

    let with_host_env = run_jiji(
        "login",
        &config_path,
        &bin,
        &["--skip-remote", "--host-env"],
        &[
            ("JIJI_TEST_LOG", log.to_str().unwrap()),
            ("REGISTRY_TOKEN", "from-host-env"),
        ],
    );
    assert!(
        with_host_env.status.success(),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&with_host_env.stdout),
        String::from_utf8_lossy(&with_host_env.stderr)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn one_unreachable_host_does_not_prevent_the_other_from_logging_in() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert(
        login_command("docker", "registry.example.com", "alice"),
        success(""),
    );
    let (harness, reachable) = spawn_test_server(client_key.public_key().clone(), responses).await;
    let unreachable = {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind throwaway listener");
        listener.local_addr().expect("read listener addr")
    };
    let config_path = write_config_str(
        dir.path(),
        &config_yaml_two_servers(reachable, unreachable, &key_path, "docker"),
    );
    let bin = write_fake_engine(dir.path(), "docker");
    let log = dir.path().join("commands.log");

    let output = run_jiji(
        "login",
        &config_path,
        &bin,
        &["--skip-local"],
        &[("JIJI_TEST_LOG", log.to_str().unwrap())],
    );
    assert!(!output.status.success(), "expected a nonzero exit overall");
    let received = harness.received.lock().unwrap().clone();
    assert!(
        received.contains(&login_command("docker", "registry.example.com", "alice")),
        "the reachable host should still have been logged in: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn both_skip_flags_fail_before_any_side_effect() {
    let (dir, key_path, client_key) = setup_test_dir();
    let (harness, addr) = spawn_test_server(client_key.public_key().clone(), HashMap::new()).await;
    let config_path = write_config_str(dir.path(), &config_yaml(addr, &key_path, "docker"));
    let bin = write_fake_engine(dir.path(), "docker");
    let log = dir.path().join("commands.log");

    let output = run_jiji(
        "login",
        &config_path,
        &bin,
        &["--skip-local", "--skip-remote"],
        &[("JIJI_TEST_LOG", log.to_str().unwrap())],
    );
    assert!(!output.status.success());
    assert!(!log.exists());
    assert!(harness.received.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn services_flag_is_rejected_before_any_side_effect() {
    let (dir, key_path, client_key) = setup_test_dir();
    let (harness, addr) = spawn_test_server(client_key.public_key().clone(), HashMap::new()).await;
    let config_path = write_config_str(dir.path(), &config_yaml(addr, &key_path, "docker"));
    let bin = write_fake_engine(dir.path(), "docker");
    let log = dir.path().join("commands.log");

    let current_path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![bin.clone()];
    paths.extend(std::env::split_paths(&current_path));
    let mut command = Command::new(env!("CARGO_BIN_EXE_jiji"));
    command
        .arg("-S")
        .arg("web")
        .arg("registry")
        .arg("login")
        .arg("-c")
        .arg(&config_path)
        .env("PATH", std::env::join_paths(paths).expect("join PATH"))
        .env("JIJI_TEST_LOG", &log);
    let output = command.output().expect("run jiji -S web registry login");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("does not accept"), "stderr: {stderr}");
    assert!(!log.exists());
    assert!(harness.received.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn local_registry_login_is_a_no_op_that_touches_neither_engine_nor_ssh() {
    let (dir, key_path, client_key) = setup_test_dir();
    let (harness, addr) = spawn_test_server(client_key.public_key().clone(), HashMap::new()).await;
    let config_path = write_config_str(dir.path(), &config_yaml_local_registry(addr, &key_path));
    let bin = write_fake_engine(dir.path(), "docker");
    let log = dir.path().join("commands.log");

    let output = run_jiji(
        "login",
        &config_path,
        &bin,
        &[],
        &[("JIJI_TEST_LOG", log.to_str().unwrap())],
    );
    assert!(output.status.success());
    assert!(
        !log.exists(),
        "local engine must not run for a local registry"
    );
    assert!(harness.received.lock().unwrap().is_empty());
}

// ---------------------------------------------------------------------------------------------
// Logout
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn default_logout_logs_out_locally_and_on_every_configured_server() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert(
        logout_command("docker", "registry.example.com"),
        success(""),
    );
    let (harness, addr) = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config_str(dir.path(), &config_yaml(addr, &key_path, "docker"));
    let bin = write_fake_engine(dir.path(), "docker");
    let log = dir.path().join("commands.log");

    let output = run_jiji(
        "logout",
        &config_path,
        &bin,
        &[],
        &[("JIJI_TEST_LOG", log.to_str().unwrap())],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stdout: {stdout} stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("completed on 2 target(s)"),
        "stdout: {stdout}"
    );
    let local_log = std::fs::read_to_string(&log).expect("read local log");
    assert!(local_log.contains("logout registry.example.com"));
    let received = harness.received.lock().unwrap().clone();
    assert!(received.contains(&logout_command("docker", "registry.example.com")));
}

#[tokio::test(flavor = "multi_thread")]
async fn already_logged_out_is_reported_as_success() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert(
        logout_command("podman", "registry.example.com"),
        failure("Error: not logged into registry.example.com\n"),
    );
    let (_harness, addr) = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config_str(dir.path(), &config_yaml(addr, &key_path, "podman"));
    let bin = write_fake_engine(dir.path(), "podman");
    let log = dir.path().join("commands.log");

    let output = run_jiji(
        "logout",
        &config_path,
        &bin,
        &["--skip-local"],
        &[
            ("JIJI_TEST_LOG", log.to_str().unwrap()),
            ("JIJI_TEST_LOGOUT_EXIT", "1"),
            (
                "JIJI_TEST_LOGOUT_STDERR",
                "Error: not logged into registry.example.com",
            ),
        ],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stdout: {stdout} stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("already logged out"), "stdout: {stdout}");
}

#[tokio::test(flavor = "multi_thread")]
async fn local_registry_logout_is_a_no_op_that_touches_neither_engine_nor_ssh() {
    let (dir, key_path, client_key) = setup_test_dir();
    let (harness, addr) = spawn_test_server(client_key.public_key().clone(), HashMap::new()).await;
    let config_path = write_config_str(dir.path(), &config_yaml_local_registry(addr, &key_path));
    let bin = write_fake_engine(dir.path(), "docker");
    let log = dir.path().join("commands.log");

    let output = run_jiji(
        "logout",
        &config_path,
        &bin,
        &[],
        &[("JIJI_TEST_LOG", log.to_str().unwrap())],
    );
    assert!(output.status.success());
    assert!(
        !log.exists(),
        "local engine must not run for a local registry"
    );
    assert!(harness.received.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn logout_both_skip_flags_fail_before_any_side_effect() {
    let (dir, key_path, client_key) = setup_test_dir();
    let (harness, addr) = spawn_test_server(client_key.public_key().clone(), HashMap::new()).await;
    let config_path = write_config_str(dir.path(), &config_yaml(addr, &key_path, "docker"));
    let bin = write_fake_engine(dir.path(), "docker");
    let log = dir.path().join("commands.log");

    let output = run_jiji(
        "logout",
        &config_path,
        &bin,
        &["--skip-local", "--skip-remote"],
        &[("JIJI_TEST_LOG", log.to_str().unwrap())],
    );
    assert!(!output.status.success());
    assert!(!log.exists());
    assert!(harness.received.lock().unwrap().is_empty());
}
