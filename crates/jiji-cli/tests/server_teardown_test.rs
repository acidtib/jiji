//! Integration tests for `jiji server teardown`, run as a real subprocess against a real,
//! in-process SSH server (mirroring `deploy_test.rs`/`server_setup_test.rs`'s pattern), so the
//! full config-load -> discover -> confirm -> per-host teardown path is exercised without
//! touching real hosts.

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

fn failure() -> CannedResponse {
    CannedResponse {
        success: false,
        stdout: String::new(),
        stderr: "no such object".to_string(),
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

/// A single service ("web", image "example/web:latest") on a single server ("app").
fn config_yaml(addr: SocketAddr, key_path: &std::path::Path, engine: &str) -> String {
    format!(
        r#"
project: demo
builder: {{ engine: {engine} }}
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
        engine = engine,
        ip = addr.ip(),
        port = addr.port(),
        key_path = key_path.display(),
    )
}

/// Two servers: "app" (reachable, hosts "web") and "data" (unreachable, no services -- teardown
/// targets every configured server regardless of service placement).
fn config_yaml_two_servers(
    reachable: SocketAddr,
    unreachable: SocketAddr,
    key_path: &std::path::Path,
    engine: &str,
) -> String {
    format!(
        r#"
project: demo
builder: {{ engine: {engine} }}
servers:
  app:
    host: {reachable_ip}
    port: {reachable_port}
    keys:
      - {key_path}
  data:
    host: {unreachable_ip}
    port: {unreachable_port}
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
        reachable_ip = reachable.ip(),
        reachable_port = reachable.port(),
        unreachable_ip = unreachable.ip(),
        unreachable_port = unreachable.port(),
        key_path = key_path.display(),
    )
}

fn write_config_str(dir: &std::path::Path, contents: &str) -> std::path::PathBuf {
    let config_path = dir.join("deploy.yml");
    std::fs::write(&config_path, contents).expect("write test deploy.yml");
    config_path
}

fn run_jiji_teardown(config_path: &std::path::Path, extra_args: &[&str]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_jiji"));
    command
        .arg("server")
        .arg("teardown")
        .arg("-c")
        .arg(config_path);
    for arg in extra_args {
        command.arg(arg);
    }
    command.output().expect("run jiji server teardown")
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

fn inspect_status_command(engine: &str, name: &str) -> String {
    format!("{engine} inspect {name} --format '{{{{.State.Status}}}}'")
}

fn remove_container_command(engine: &str, name: &str) -> String {
    format!("{engine} rm -f {name}")
}

fn list_managed_containers_command(engine: &str, project: &str) -> String {
    format!(
        "{engine} ps -a --filter label=jiji.managed=true --filter label=jiji.project={project} --format '{{{{.Names}}}}|{{{{.Label \"jiji.project\"}}}}|{{{{.Label \"jiji.service\"}}}}|{{{{.Label \"jiji.server\"}}}}|{{{{.State}}}}'"
    )
}

fn list_other_project_containers_command(engine: &str) -> String {
    format!(
        "{engine} ps -a --filter label=jiji.managed=true --format '{{{{.Names}}}}|{{{{.Label \"jiji.project\"}}}}|{{{{.Label \"jiji.service\"}}}}|{{{{.Label \"jiji.server\"}}}}|{{{{.State}}}}'"
    )
}

/// Sets up the two canned responses shared by every "one healthy container on 'app'" scenario:
/// the label-filtered listing that discovers it, and the inspect that reports it running.
fn one_container_responses(engine: &str) -> HashMap<String, CannedResponse> {
    let mut responses = HashMap::new();
    responses.insert(
        list_managed_containers_command(engine, "demo"),
        success("demo-web-a|demo|web|app|running\n"),
    );
    responses.insert(
        inspect_status_command(engine, "demo-web-a"),
        success("running\n"),
    );
    responses
}

#[tokio::test(flavor = "multi_thread")]
async fn full_successful_teardown_reports_fully_torn_down() {
    let (dir, key_path, client_key) = setup_test_dir();
    let responses = one_container_responses("docker");
    let (harness, addr) = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config_str(dir.path(), &config_yaml(addr, &key_path, "docker"));

    let output = run_jiji_teardown(&config_path, &["-y"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "expected success, stdout: {stdout} stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("fully torn down"), "stdout: {stdout}");

    let received = harness.received.lock().unwrap().clone();
    assert!(
        received.contains(&remove_container_command("docker", "demo-web-a")),
        "the discovered container should have been removed: {received:?}"
    );
    assert!(
        received
            .iter()
            .any(|c| c.contains("rm -rf /etc/jiji/network")),
        "compiled network state should have been removed: {received:?}"
    );
    assert!(
        received
            .iter()
            .any(|c| c.contains("volume rm kamal-proxy-config")),
        "kamal-proxy's now-orphaned config volume should have been removed: {received:?}"
    );
    assert!(
        received
            .iter()
            .any(|c| c.contains("rm -rf /etc/jiji/certs")),
        "kamal-proxy's now-orphaned certs directory should have been removed: {received:?}"
    );
    assert!(
        received.iter().any(|c| c.contains("rm -rf .jiji/demo")),
        "the project's staged env/mount directory should have been removed: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn dry_run_sends_zero_mutating_commands() {
    let (dir, key_path, client_key) = setup_test_dir();
    let responses = one_container_responses("docker");
    let (harness, addr) = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config_str(dir.path(), &config_yaml(addr, &key_path, "docker"));

    let output = run_jiji_teardown(&config_path, &["--dry-run"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "expected success, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("Dry run"), "stdout: {stdout}");

    let received = harness.received.lock().unwrap().clone();
    let mutating_substrings = [
        "rm -f",
        "rm -rf",
        "rmi ",
        "volume rm",
        "network rm",
        "systemctl disable",
        "nft delete",
    ];
    for command in &received {
        for marker in mutating_substrings {
            assert!(
                !command.contains(marker),
                "dry run must never send a mutating command, found: {command}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn container_removal_failure_prevents_network_removal() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = one_container_responses("docker");
    responses.insert(remove_container_command("docker", "demo-web-a"), failure());
    let (harness, addr) = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config_str(dir.path(), &config_yaml(addr, &key_path, "docker"));

    let output = run_jiji_teardown(&config_path, &["-y"]);
    assert!(!output.status.success(), "expected non-zero exit");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("partially torn down"),
        "stderr: {stderr} stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    let received = harness.received.lock().unwrap().clone();
    assert!(
        !received
            .iter()
            .any(|c| c.contains("rm -rf /etc/jiji/network")),
        "network layer must not be torn down after an application-layer failure: {received:?}"
    );
    assert!(
        !received.iter().any(|c| c.contains("systemctl disable")),
        "network units must not be touched after an application-layer failure: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn another_projects_container_blocks_the_host_entirely() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert(
        list_managed_containers_command("docker", "demo"),
        success(""),
    );
    responses.insert(
        list_other_project_containers_command("docker"),
        success("other-web-a|other|web|app|running\n"),
    );
    let (harness, addr) = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config_str(dir.path(), &config_yaml(addr, &key_path, "docker"));

    let output = run_jiji_teardown(&config_path, &["-y"]);
    assert!(!output.status.success(), "expected non-zero exit");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("blocked"), "stdout: {stdout}");

    let received = harness.received.lock().unwrap().clone();
    assert!(
        !received
            .iter()
            .any(|c| c.contains("rm -f") || c.contains("network rm")),
        "a blocked host must never receive a destructive command: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn one_unreachable_host_does_not_hide_a_successful_host() {
    let (dir, key_path, client_key) = setup_test_dir();
    let responses = one_container_responses("docker");
    let (harness, reachable_addr) =
        spawn_test_server(client_key.public_key().clone(), responses).await;

    // Bind and immediately drop, to get a port nothing is listening on.
    let unreachable_addr = {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind throwaway listener");
        listener.local_addr().expect("read listener addr")
    };

    let config_path = write_config_str(
        dir.path(),
        &config_yaml_two_servers(reachable_addr, unreachable_addr, &key_path, "docker"),
    );

    let output = run_jiji_teardown(&config_path, &["-y"]);
    assert!(!output.status.success(), "expected non-zero exit overall");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.contains("fully torn down"), "stdout: {stdout}");
    assert!(
        stderr.contains("unreachable") || stdout.contains("unreachable"),
        "stdout: {stdout} stderr: {stderr}"
    );

    let received = harness.received.lock().unwrap().clone();
    assert!(
        received.contains(&remove_container_command("docker", "demo-web-a")),
        "the reachable host's container should still have been removed: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn engine_flag_bails_before_any_ssh_command() {
    let (dir, key_path, client_key) = setup_test_dir();
    let (harness, addr) = spawn_test_server(client_key.public_key().clone(), HashMap::new()).await;
    let config_path = write_config_str(dir.path(), &config_yaml(addr, &key_path, "docker"));

    let output = run_jiji_teardown(&config_path, &["-y", "--engine"]);
    assert!(!output.status.success(), "expected non-zero exit");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not implemented yet"), "stderr: {stderr}");

    let received = harness.received.lock().unwrap().clone();
    assert!(
        received.is_empty(),
        "expected zero SSH commands: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn services_flag_is_rejected_before_any_ssh_command() {
    let (dir, key_path, client_key) = setup_test_dir();
    let (harness, addr) = spawn_test_server(client_key.public_key().clone(), HashMap::new()).await;
    let config_path = write_config_str(dir.path(), &config_yaml(addr, &key_path, "docker"));

    let mut command = Command::new(env!("CARGO_BIN_EXE_jiji"));
    command
        .arg("-S")
        .arg("web")
        .arg("server")
        .arg("teardown")
        .arg("-c")
        .arg(&config_path)
        .arg("-y");
    let output = command.output().expect("run jiji server teardown -S web");

    assert!(!output.status.success(), "expected non-zero exit");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("does not accept"), "stderr: {stderr}");

    let received = harness.received.lock().unwrap().clone();
    assert!(
        received.is_empty(),
        "expected zero SSH commands: {received:?}"
    );
}
