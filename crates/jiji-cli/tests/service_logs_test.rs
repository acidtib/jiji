//! Integration tests for `jiji service logs`, run as a real subprocess against a real, in-process
//! SSH server (mirroring `server_exec_test.rs`'s minimal harness, which needs no port-forwarding
//! support).

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
    authorized_key: PublicKey,
    responses: HashMap<String, CannedResponse>,
) -> (Harness, SocketAddr) {
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

fn config_yaml_two_servers(key_path: &std::path::Path) -> String {
    format!(
        r#"
project: demo
builder: {{ engine: docker }}
servers:
  app1:
    host: 192.0.2.1
    keys:
      - {key_path}
  app2:
    host: 192.0.2.2
    keys:
      - {key_path}
services:
  web:
    image: example/web:latest
    hosts: [app1, app2]
ssh:
  user: tester
  keys_only: true
  connect_timeout: 2
"#,
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

fn run_jiji_service_logs(
    config_path: &std::path::Path,
    extra_args: &[&str],
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_jiji"));
    command
        .arg("service")
        .arg("logs")
        .arg("-c")
        .arg(config_path);
    for arg in extra_args {
        command.arg(arg);
    }
    command.output().expect("run jiji service logs")
}

fn active_slots_path() -> String {
    format!(
        "cat /etc/jiji/network/{}/service-nat-current/active-slots",
        jiji_network::systemd_unit_slug("demo")
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn logs_reads_the_active_slot_container_for_a_configured_service() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert(active_slots_path(), success("demo:web:app=a\n"));
    responses.insert(
        "docker logs --timestamps --tail=100 demo-web-a".to_string(),
        success("hello from web\n"),
    );

    let (harness, addr) = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config_str(dir.path(), &config_yaml(addr, &key_path));

    let output = run_jiji_service_logs(&config_path, &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(stdout.contains("hello from web"), "stdout: {stdout}");

    let received = harness.received.lock().unwrap().clone();
    assert!(received.contains(&active_slots_path()));
    assert!(received
        .iter()
        .any(|c| c == "docker logs --timestamps --tail=100 demo-web-a"));
}

#[tokio::test(flavor = "multi_thread")]
async fn logs_skips_a_service_with_no_active_container_instead_of_failing() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert(active_slots_path(), success(""));

    let (_harness, addr) = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config_str(dir.path(), &config_yaml(addr, &key_path));

    let output = run_jiji_service_logs(&config_path, &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(stdout.contains("no active container"), "stdout: {stdout}");
}

#[tokio::test(flavor = "multi_thread")]
async fn logs_container_id_bypasses_active_slot_resolution() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert(
        "docker logs --timestamps --tail=100 arbitrary-container".to_string(),
        success("raw container output\n"),
    );

    let (harness, addr) = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config_str(dir.path(), &config_yaml(addr, &key_path));

    let output = run_jiji_service_logs(&config_path, &["--container-id", "arbitrary-container"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(stdout.contains("raw container output"), "stdout: {stdout}");

    let received = harness.received.lock().unwrap().clone();
    assert!(
        !received.contains(&active_slots_path()),
        "--container-id must not resolve an active slot: {received:?}"
    );
}

#[test]
fn container_id_rejects_services_filter_before_loading_configuration() {
    let output = Command::new(env!("CARGO_BIN_EXE_jiji"))
        .args([
            "-S",
            "web",
            "-c",
            "/does/not/exist",
            "service",
            "logs",
            "--container-id",
            "foo",
        ])
        .output()
        .expect("run jiji service logs");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("does not accept -S/--services"), "{stderr}");
}

#[test]
fn follow_rejects_multiple_targets_before_connecting() {
    let dir = tempfile::tempdir().expect("create tempdir");
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
    let config_path = write_config_str(dir.path(), &config_yaml_two_servers(&key_path));

    let output = run_jiji_service_logs(&config_path, &["--follow"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("requires exactly one"), "{stderr}");
}
