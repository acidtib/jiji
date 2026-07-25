//! Integration tests for `jiji server exec`'s non-interactive path (single host and multi-host),
//! run as a real subprocess against a real, in-process SSH server (mirroring
//! `server_teardown_test.rs`'s pattern). Interactive/PTY behavior is not covered here: there is
//! no real terminal available to a test subprocess's captured pipes, so that path is exercised
//! only through live validation.

use std::collections::HashMap;
use std::net::SocketAddr;
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
    received: Arc<Mutex<Vec<String>>>,
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
}

async fn spawn_test_server(
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
    let mut test_server = TestServer {
        authorized_key,
        responses: Arc::new(responses),
        pending: Arc::new(Mutex::new(HashMap::new())),
        received: received.clone(),
    };

    tokio::spawn(async move {
        let _ = test_server.run_on_socket(config, &listener).await;
        drop(listener);
    });

    (Harness { received }, addr)
}

fn config_yaml(addr: SocketAddr, key_path: &std::path::Path) -> String {
    format!(
        r#"
project: demo
builder: {{ engine: docker }}
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
        ip = addr.ip(),
        port = addr.port(),
        key_path = key_path.display(),
    )
}

fn config_yaml_two_servers(
    addr1: SocketAddr,
    addr2: SocketAddr,
    key_path: &std::path::Path,
) -> String {
    format!(
        r#"
project: demo
builder: {{ engine: docker }}
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

fn run_jiji_exec(config_path: &std::path::Path, extra_args: &[&str]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_jiji"));
    command.arg("server").arg("exec").arg("-c").arg(config_path);
    for arg in extra_args {
        command.arg(arg);
    }
    // No controlling terminal in a captured-pipe subprocess, so this always takes the
    // non-interactive path regardless of --interactive.
    command.output().expect("run jiji server exec")
}

#[tokio::test(flavor = "multi_thread")]
async fn non_interactive_command_streams_output_and_succeeds() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert("echo hi".to_string(), success("hi\n"));
    let (harness, addr) =
        spawn_test_server("127.0.0.1", client_key.public_key().clone(), responses).await;
    let config_path = write_config_str(dir.path(), &config_yaml(addr, &key_path));

    let output = run_jiji_exec(&config_path, &["echo hi"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stdout: {stdout} stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("hi"), "stdout: {stdout}");

    let received = harness.received.lock().unwrap().clone();
    assert!(received.contains(&"echo hi".to_string()));
}

#[tokio::test(flavor = "multi_thread")]
async fn non_interactive_command_propagates_nonzero_exit() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert("false".to_string(), failure("boom"));
    let (_harness, addr) =
        spawn_test_server("127.0.0.1", client_key.public_key().clone(), responses).await;
    let config_path = write_config_str(dir.path(), &config_yaml(addr, &key_path));

    let output = run_jiji_exec(&config_path, &["false"]);
    assert!(!output.status.success(), "expected a nonzero exit");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("exited with status"), "stderr: {stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn unmatched_host_filter_fails_before_connecting() {
    let (dir, key_path, client_key) = setup_test_dir();
    let (harness, addr) =
        spawn_test_server("127.0.0.1", client_key.public_key().clone(), HashMap::new()).await;
    let config_path = write_config_str(dir.path(), &config_yaml(addr, &key_path));

    let output = run_jiji_exec(&config_path, &["echo hi", "-H", "does-not-exist"]);
    assert!(!output.status.success());
    assert!(harness.received.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn interactive_shell_with_multiple_matched_hosts_is_rejected() {
    let (dir, key_path, client_key) = setup_test_dir();
    let (harness1, addr1) =
        spawn_test_server("127.0.0.1", client_key.public_key().clone(), HashMap::new()).await;
    let (_harness2, addr2) =
        spawn_test_server("127.0.0.2", client_key.public_key().clone(), HashMap::new()).await;
    let config_path = write_config_str(
        dir.path(),
        &config_yaml_two_servers(addr1, addr2, &key_path),
    );

    // No command given -> implies an interactive login shell, which is bound to one host.
    let output = run_jiji_exec(&config_path, &[]);
    assert!(
        !output.status.success(),
        "expected exactly-one-host rejection"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("interactive session targets exactly one host"),
        "stderr: {stderr}"
    );
    assert!(harness1.received.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn interactive_flag_with_multiple_matched_hosts_is_rejected() {
    let (dir, key_path, client_key) = setup_test_dir();
    let (harness1, addr1) =
        spawn_test_server("127.0.0.1", client_key.public_key().clone(), HashMap::new()).await;
    let (_harness2, addr2) =
        spawn_test_server("127.0.0.2", client_key.public_key().clone(), HashMap::new()).await;
    let config_path = write_config_str(
        dir.path(),
        &config_yaml_two_servers(addr1, addr2, &key_path),
    );

    let output = run_jiji_exec(&config_path, &["echo hi", "--interactive"]);
    assert!(
        !output.status.success(),
        "expected exactly-one-host rejection"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("interactive session targets exactly one host"),
        "stderr: {stderr}"
    );
    assert!(harness1.received.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn non_interactive_command_runs_concurrently_on_every_matched_host() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert("echo hi".to_string(), success("hi\n"));
    let (harness1, addr1) = spawn_test_server(
        "127.0.0.1",
        client_key.public_key().clone(),
        responses.clone(),
    )
    .await;
    let (harness2, addr2) =
        spawn_test_server("127.0.0.2", client_key.public_key().clone(), responses).await;
    let config_path = write_config_str(
        dir.path(),
        &config_yaml_two_servers(addr1, addr2, &key_path),
    );

    // No -H filter matches both app1 and app2.
    let output = run_jiji_exec(&config_path, &["echo hi"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stdout: {stdout} stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(harness1
        .received
        .lock()
        .unwrap()
        .contains(&"echo hi".to_string()));
    assert!(harness2
        .received
        .lock()
        .unwrap()
        .contains(&"echo hi".to_string()));
}

#[tokio::test(flavor = "multi_thread")]
async fn sequential_flag_still_runs_the_command_on_every_matched_host() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert("echo hi".to_string(), success("hi\n"));
    let (harness1, addr1) = spawn_test_server(
        "127.0.0.1",
        client_key.public_key().clone(),
        responses.clone(),
    )
    .await;
    let (harness2, addr2) =
        spawn_test_server("127.0.0.2", client_key.public_key().clone(), responses).await;
    let config_path = write_config_str(
        dir.path(),
        &config_yaml_two_servers(addr1, addr2, &key_path),
    );

    let output = run_jiji_exec(&config_path, &["echo hi", "--sequential"]);
    assert!(
        output.status.success(),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(harness1
        .received
        .lock()
        .unwrap()
        .contains(&"echo hi".to_string()));
    assert!(harness2
        .received
        .lock()
        .unwrap()
        .contains(&"echo hi".to_string()));
}

#[tokio::test(flavor = "multi_thread")]
async fn multi_host_command_reports_which_hosts_failed() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut ok_responses = HashMap::new();
    ok_responses.insert("false".to_string(), success("ignored"));
    let mut fail_responses = HashMap::new();
    fail_responses.insert("false".to_string(), failure("boom"));
    let (_harness1, addr1) =
        spawn_test_server("127.0.0.1", client_key.public_key().clone(), ok_responses).await;
    let (_harness2, addr2) =
        spawn_test_server("127.0.0.2", client_key.public_key().clone(), fail_responses).await;
    let config_path = write_config_str(
        dir.path(),
        &config_yaml_two_servers(addr1, addr2, &key_path),
    );

    let output = run_jiji_exec(&config_path, &["false"]);
    assert!(!output.status.success(), "expected a nonzero exit");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Command failed on 1 server"),
        "stderr: {stderr}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn services_flag_is_rejected_before_any_side_effect() {
    let (dir, key_path, client_key) = setup_test_dir();
    let (harness, addr) =
        spawn_test_server("127.0.0.1", client_key.public_key().clone(), HashMap::new()).await;
    let config_path = write_config_str(dir.path(), &config_yaml(addr, &key_path));

    let mut command = Command::new(env!("CARGO_BIN_EXE_jiji"));
    command
        .arg("-S")
        .arg("web")
        .arg("server")
        .arg("exec")
        .arg("-c")
        .arg(&config_path)
        .arg("echo hi");
    let output = command.output().expect("run jiji -S web server exec");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("does not accept"), "stderr: {stderr}");
    assert!(harness.received.lock().unwrap().is_empty());
}
