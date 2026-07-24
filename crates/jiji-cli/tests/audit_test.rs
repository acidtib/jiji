//! Integration tests for `jiji audit` and its write side (wired into `jiji lock acquire`/
//! `release` and `jiji deploy`), run as a real subprocess against a real, in-process SSH server
//! (mirroring `lock_commands_test.rs`'s pattern). The audit trail is host-scoped, like locks, so
//! there is no network-generation reconciliation or endpoint selection involved for the read side.

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

        let occurrence = self
            .received
            .lock()
            .expect("received mutex poisoned")
            .iter()
            .filter(|received| *received == &command)
            .count();
        let response = self
            .responses
            .get(&format!("{command}#{occurrence}"))
            .or_else(|| self.responses.get(&command))
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
    addr: SocketAddr,
    received: Arc<Mutex<Vec<String>>>,
}

async fn spawn_test_server(
    authorized_key: PublicKey,
    responses: HashMap<String, CannedResponse>,
) -> Harness {
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

    Harness { addr, received }
}

/// A single server ("app"), no services -- audit's read side never touches `services:`.
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
services: {{}}
ssh:
  user: tester
  keys_only: true
"#,
        ip = addr.ip(),
        port = addr.port(),
        key_path = key_path.display(),
    )
}

fn write_config(
    dir: &std::path::Path,
    addr: SocketAddr,
    key_path: &std::path::Path,
) -> std::path::PathBuf {
    let config_path = dir.join("deploy.yml");
    std::fs::write(&config_path, config_yaml(addr, key_path)).expect("write test deploy.yml");
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

const AUDIT_READ: &str = "tail -n 20 .jiji/demo/audit.log 2>/dev/null || true";
const LOCK_PATH: &str = "cat .jiji/demo/deploy.lock 2>/dev/null || true";

fn run_jiji_audit(config_path: &std::path::Path, args: &[&str]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_jiji"));
    command.arg("-c").arg(config_path).arg("audit");
    for arg in args {
        command.arg(arg);
    }
    command.output().expect("run jiji audit")
}

fn run_jiji_lock(config_path: &std::path::Path, args: &[&str]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_jiji"));
    command.arg("-c").arg(config_path).arg("lock");
    for arg in args {
        command.arg(arg);
    }
    command.output().expect("run jiji lock")
}

#[test]
fn rejects_service_filter_before_loading_configuration() {
    let output = Command::new(env!("CARGO_BIN_EXE_jiji"))
        .args(["-S", "web", "-c", "/does/not/exist", "audit"])
        .output()
        .expect("run jiji audit");

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("does not accept -S/--services"));
}

#[tokio::test(flavor = "multi_thread")]
async fn audit_prints_entries_from_the_remote_log() {
    let (dir, key_path, client_key) = setup_test_dir();
    let body = "{\"timestamp\":1000000000,\"action\":\"deploy\",\"status\":\"success\",\"actor\":\"tester\",\"message\":\"demo:web:app deployed (slot a)\"}\n\
                {\"timestamp\":1000000100,\"action\":\"lock_acquire\",\"status\":\"success\",\"message\":\"\\\"deploying\\\" by tester\",\"actor\":\"tester\"}\n";
    let mut responses = HashMap::new();
    responses.insert(AUDIT_READ.to_string(), success(body));

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);

    let output = run_jiji_audit(&config_path, &[]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("deploy: demo:web:app deployed (slot a)"),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("lock_acquire"), "stdout: {stdout}");
    assert!(stdout.contains("[SUCCESS]"), "stdout: {stdout}");
}

#[tokio::test(flavor = "multi_thread")]
async fn audit_grep_and_status_filters_narrow_the_results() {
    let (dir, key_path, client_key) = setup_test_dir();
    let body = "{\"timestamp\":1000000000,\"action\":\"deploy\",\"status\":\"success\",\"actor\":\"tester\",\"message\":\"demo:web:app deployed\"}\n\
                {\"timestamp\":1000000100,\"action\":\"deploy\",\"status\":\"failed\",\"actor\":\"tester\",\"message\":\"demo:api:app failed\"}\n";
    let mut responses = HashMap::new();
    responses.insert(AUDIT_READ.to_string(), success(body));

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);

    let output = run_jiji_audit(&config_path, &["--status", "failed"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("demo:api:app failed"), "stdout: {stdout}");
    assert!(
        !stdout.contains("demo:web:app deployed"),
        "stdout: {stdout}"
    );

    let output = run_jiji_audit(&config_path, &["--grep", "web"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("demo:web:app deployed"), "stdout: {stdout}");
    assert!(!stdout.contains("demo:api:app failed"), "stdout: {stdout}");
}

#[tokio::test(flavor = "multi_thread")]
async fn audit_json_prints_one_object_per_line_with_a_host_field() {
    let (dir, key_path, client_key) = setup_test_dir();
    let body = "{\"timestamp\":1000000000,\"action\":\"deploy\",\"status\":\"success\",\"actor\":\"tester\",\"message\":\"ok\"}\n";
    let mut responses = HashMap::new();
    responses.insert(AUDIT_READ.to_string(), success(body));

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);

    let output = run_jiji_audit(&config_path, &["--json"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().next().expect("at least one json line");
    let parsed: serde_json::Value = serde_json::from_str(line).expect("valid json line");
    assert_eq!(parsed["host"], "app");
    assert_eq!(parsed["action"], "deploy");
}

#[tokio::test(flavor = "multi_thread")]
async fn audit_skips_malformed_lines_instead_of_failing() {
    let (dir, key_path, client_key) = setup_test_dir();
    let body = "not json\n{\"timestamp\":1000000000,\"action\":\"deploy\",\"status\":\"success\",\"actor\":\"tester\",\"message\":\"ok\"}\n";
    let mut responses = HashMap::new();
    responses.insert(AUDIT_READ.to_string(), success(body));

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);

    let output = run_jiji_audit(&config_path, &[]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("deploy: ok"));
}

#[tokio::test(flavor = "multi_thread")]
async fn lock_acquire_writes_an_audit_entry() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert(LOCK_PATH.to_string(), success(""));

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);

    let acquire = run_jiji_lock(&config_path, &["acquire", "Deploying v1.2.3"]);
    assert!(
        acquire.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&acquire.stderr)
    );

    let received = harness.received.lock().unwrap().clone();
    assert!(
        received
            .iter()
            .any(|c| c.contains("cat >> .jiji/demo/audit.log")
                && c.contains("install -d -m 0700 .jiji/demo")),
        "acquire should append an audit entry: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn lock_release_writes_an_audit_entry() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert(
        LOCK_PATH.to_string(),
        success(r#"{"message":"deploying","acquired_at":1000000000,"acquired_by":"bob","pid":42}"#),
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);

    let release = run_jiji_lock(&config_path, &["release"]);
    assert!(
        release.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&release.stderr)
    );

    let received = harness.received.lock().unwrap().clone();
    assert!(
        received
            .iter()
            .any(|c| c.contains("cat >> .jiji/demo/audit.log")),
        "release should append an audit entry: {received:?}"
    );
}
