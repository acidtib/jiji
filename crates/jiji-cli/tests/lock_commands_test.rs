//! Integration tests for `jiji lock`, run as a real subprocess against a real, in-process SSH
//! server (mirroring `service_restart_test.rs`'s pattern). Locks are host-scoped, so unlike
//! `deploy`/`service restart` there is no network-generation reconciliation or endpoint selection
//! involved -- just config load, host selection, connect, and remote lock-file commands.

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

/// A single server ("app"), no services -- lock commands never touch `services:`.
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

const LOCK_PATH: &str = "cat .jiji/demo/deploy.lock 2>/dev/null || true";

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
        .args(["-S", "web", "-c", "/does/not/exist", "lock", "status"])
        .output()
        .expect("run jiji lock status");

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("does not accept -S/--services"));
}

#[tokio::test(flavor = "multi_thread")]
async fn acquire_writes_a_lock_file_that_status_and_show_then_report() {
    let (dir, key_path, client_key) = setup_test_dir();
    // The mock server can't actually persist a written lock file, so each read is canned by call
    // order: #1 is `acquire`'s pre-write check (must see unlocked), #2/#3 are the later `status`
    // and `show` reads (must see what `acquire` would have written).
    let locked_body =
        r#"{"message":"Deploying v1.2.3","acquired_at":1000000000,"acquired_by":"tester","pid":1}"#;
    let mut responses = HashMap::new();
    responses.insert(format!("{LOCK_PATH}#1"), success(""));
    responses.insert(format!("{LOCK_PATH}#2"), success(locked_body));
    responses.insert(format!("{LOCK_PATH}#3"), success(locked_body));

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
            .any(|c| c.contains("mkdir .jiji/demo/deploy.lock")
                && c.contains("install -m 0600 /dev/stdin")),
        "lock file should have been written atomically: {received:?}"
    );

    let status = run_jiji_lock(&config_path, &["status", "--json"]);
    assert!(status.status.success());
    let stdout = String::from_utf8_lossy(&status.stdout);
    assert!(stdout.contains("\"locked\": true"), "stdout: {stdout}");
    assert!(stdout.contains("Deploying v1.2.3"), "stdout: {stdout}");

    let show = run_jiji_lock(&config_path, &["show"]);
    assert!(show.status.success());
    let stdout = String::from_utf8_lossy(&show.stdout);
    assert!(stdout.contains("LOCKED"), "stdout: {stdout}");
    assert!(stdout.contains("Deploying v1.2.3"), "stdout: {stdout}");
}

#[tokio::test(flavor = "multi_thread")]
async fn acquire_without_force_fails_fast_when_already_locked_and_force_overrides() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert(
        LOCK_PATH.to_string(),
        success(r#"{"message":"existing deploy","acquired_at":1000000000,"acquired_by":"bob","pid":42}"#),
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);

    let blocked = run_jiji_lock(&config_path, &["acquire", "new deploy", "--timeout", "1"]);
    assert!(!blocked.status.success());
    let stderr = String::from_utf8_lossy(&blocked.stderr);
    assert!(stderr.contains("Timed out"), "stderr: {stderr}");
    assert!(stderr.contains("existing deploy"), "stderr: {stderr}");
    assert!(
        !harness
            .received
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.contains("mkdir .jiji/demo/deploy.lock")),
        "a blocked acquire must never write a lock file"
    );

    let forced = run_jiji_lock(&config_path, &["acquire", "new deploy", "--force"]);
    assert!(
        forced.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&forced.stderr)
    );
    assert!(
        harness
            .received
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.contains("mkdir .jiji/demo/deploy.lock")),
        "--force must still write the new lock file"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn release_removes_the_lock_file() {
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
        received.iter().any(|command| {
            command.contains("rm -f .jiji/demo/deploy.lock/info.json")
                && command.contains("rmdir .jiji/demo/deploy.lock")
        }),
        "received: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn release_warns_and_succeeds_when_nothing_is_locked() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert(LOCK_PATH.to_string(), success(""));

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);

    let release = run_jiji_lock(&config_path, &["release"]);
    assert!(release.status.success());
    assert!(String::from_utf8_lossy(&release.stdout).contains("No deployment locks found"));
}
