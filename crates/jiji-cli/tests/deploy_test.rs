//! Integration tests for `jiji deploy`, run as a real subprocess against a real, in-process SSH
//! server (mirroring `server_setup_test.rs`'s pattern), so the full config-load -> plan ->
//! connect -> per-endpoint deploy transaction path is exercised without touching real hosts.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jiji_config::Config;
use jiji_network::NetworkPlanner;
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
    /// Every command received, in order -- lets tests assert on absence/ordering, not just on
    /// the final canned outcome of one command.
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

fn write_config(
    dir: &std::path::Path,
    addr: SocketAddr,
    key_path: &std::path::Path,
    engine: &str,
) -> std::path::PathBuf {
    let config_path = dir.join("deploy.yml");
    std::fs::write(&config_path, config_yaml(addr, key_path, engine))
        .expect("write test deploy.yml");
    config_path
}

fn plan_generation(addr: SocketAddr, engine: &str) -> String {
    let yaml = config_yaml(addr, std::path::Path::new("/dev/null"), engine);
    let config: Config = serde_yaml::from_str(&yaml).expect("parse test config");
    let plan = NetworkPlanner::new()
        .plan(&config)
        .expect("build test plan");
    plan.generation
}

fn run_jiji_deploy(config_path: &std::path::Path, extra_args: &[&str]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_jiji"));
    command.arg("deploy").arg("-c").arg(config_path);
    for arg in extra_args {
        command.arg(arg);
    }
    command.output().expect("run jiji deploy")
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

const ACTIVE_SLOTS_PATH: &str = "cat /etc/jiji/network/service-nat-current/active-slots";
const GENERATION_PATH: &str = "cat /etc/jiji/network/generation 2>/dev/null || true";
const MKTEMP_COMMAND: &str = "mktemp -d /etc/jiji/network/service-nat-generations/cutover.XXXXXX";

fn inspect_status_command(engine: &str, name: &str) -> String {
    format!("{engine} inspect {name} --format '{{{{.State.Status}}}}'")
}

fn readiness_health_command(engine: &str, name: &str) -> String {
    format!("{engine} inspect {name} --format '{{{{.State.Status}}}}' | grep -qx running")
}

fn image_inspect_command(engine: &str, image: &str) -> String {
    format!("{engine} image inspect {image} >/dev/null 2>&1")
}

#[tokio::test(flavor = "multi_thread")]
async fn network_generation_mismatch_blocks_before_any_container_commands() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert(GENERATION_PATH.to_string(), success("stale-generation\n"));

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path, "docker");

    let output = run_jiji_deploy(&config_path, &[]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success(), "expected non-zero exit");
    assert!(stderr.contains("network generation"), "stderr: {stderr}");
    assert!(stderr.contains("stale-generation"), "stderr: {stderr}");

    let received = harness.received.lock().unwrap().clone();
    assert!(received.contains(&GENERATION_PATH.to_string()));
    assert!(
        !received.iter().any(|c| c.contains("run --name")),
        "no container should have been created: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn first_deployment_creates_the_candidate_and_removes_nothing() {
    let (dir, key_path, client_key) = setup_test_dir();
    let generation = plan_generation(SocketAddr::from(([127, 0, 0, 1], 0)), "docker");
    // Port is not part of the generation checksum's identity inputs (server host/service/project
    // are), so a throwaway address is fine for computing the expected generation string.

    let candidate_name = "demo-web-a";
    let mut responses = HashMap::new();
    responses.insert(
        GENERATION_PATH.to_string(),
        success(&format!("{generation}\n")),
    );
    responses.insert(ACTIVE_SLOTS_PATH.to_string(), success(""));
    responses.insert(inspect_status_command("docker", candidate_name), failure());
    responses.insert(
        image_inspect_command("docker", "example/web:latest"),
        success(""),
    );
    responses.insert(
        readiness_health_command("docker", candidate_name),
        success(""),
    );
    responses.insert(
        MKTEMP_COMMAND.to_string(),
        success("/etc/jiji/network/service-nat-generations/cutover.abc123\n"),
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path, "docker");

    let output = run_jiji_deploy(&config_path, &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "expected success, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("demo:web:app: deployed"),
        "stdout: {stdout}"
    );

    let received = harness.received.lock().unwrap().clone();
    assert!(
        received
            .iter()
            .any(|c| c.contains("docker run") && c.contains(candidate_name)),
        "candidate should have been created: {received:?}"
    );
    assert!(
        !received.iter().any(|c| c.contains("rm -f")),
        "nothing should be removed on a first deployment: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn replacement_removes_the_old_container_only_after_health_and_commit_succeed() {
    let (dir, key_path, client_key) = setup_test_dir();
    let generation = plan_generation(SocketAddr::from(([127, 0, 0, 1], 0)), "docker");

    let old_name = "demo-web-a";
    let candidate_name = "demo-web-b";
    let mut responses = HashMap::new();
    responses.insert(
        GENERATION_PATH.to_string(),
        success(&format!("{generation}\n")),
    );
    responses.insert(ACTIVE_SLOTS_PATH.to_string(), success("demo:web:app=a\n"));
    responses.insert(
        inspect_status_command("docker", old_name),
        success("running\n"),
    );
    responses.insert(inspect_status_command("docker", candidate_name), failure());
    responses.insert(
        image_inspect_command("docker", "example/web:latest"),
        success(""),
    );
    responses.insert(
        readiness_health_command("docker", candidate_name),
        success(""),
    );
    responses.insert(
        MKTEMP_COMMAND.to_string(),
        success("/etc/jiji/network/service-nat-generations/cutover.def456\n"),
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path, "docker");

    let output = run_jiji_deploy(&config_path, &[]);
    assert!(
        output.status.success(),
        "expected success, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let received = harness.received.lock().unwrap().clone();
    let run_index = received
        .iter()
        .position(|c| c.contains("docker run") && c.contains(candidate_name))
        .expect("candidate should have been created");
    let remove_index = received
        .iter()
        .position(|c| c.contains(&format!("rm -f {old_name}")))
        .expect("old container should eventually be removed");
    assert!(
        run_index < remove_index,
        "candidate must be created before the old container is removed: {received:?}"
    );
    assert!(
        !received
            .iter()
            .any(|c| c.contains(&format!("rm -f {candidate_name}"))),
        "the healthy candidate itself must never be removed: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn health_check_failure_removes_only_the_candidate_and_keeps_old_container_serving() {
    let (dir, key_path, client_key) = setup_test_dir();
    let generation = plan_generation(SocketAddr::from(([127, 0, 0, 1], 0)), "docker");

    let old_name = "demo-web-a";
    let candidate_name = "demo-web-b";
    let mut responses = HashMap::new();
    responses.insert(
        GENERATION_PATH.to_string(),
        success(&format!("{generation}\n")),
    );
    responses.insert(ACTIVE_SLOTS_PATH.to_string(), success("demo:web:app=a\n"));
    responses.insert(
        inspect_status_command("docker", old_name),
        success("running\n"),
    );
    responses.insert(inspect_status_command("docker", candidate_name), failure());
    responses.insert(
        image_inspect_command("docker", "example/web:latest"),
        success(""),
    );
    responses.insert(
        readiness_health_command("docker", candidate_name),
        failure(),
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path, "docker");

    let output = run_jiji_deploy(&config_path, &[]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "expected non-zero exit");
    assert!(
        stderr.contains("previous version is still serving traffic"),
        "stderr: {stderr}"
    );

    let received = harness.received.lock().unwrap().clone();
    assert!(
        received
            .iter()
            .any(|c| c.contains(&format!("rm -f {candidate_name}"))),
        "the unhealthy candidate should be removed: {received:?}"
    );
    assert!(
        !received
            .iter()
            .any(|c| c.contains(&format!("rm -f {old_name}"))
                || c.contains(&format!("stop {old_name}"))),
        "the old container must never be touched: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn podman_first_deployment_uses_podman_commands_only() {
    let (dir, key_path, client_key) = setup_test_dir();
    let generation = plan_generation(SocketAddr::from(([127, 0, 0, 1], 0)), "podman");

    let candidate_name = "demo-web-a";
    let mut responses = HashMap::new();
    responses.insert(
        GENERATION_PATH.to_string(),
        success(&format!("{generation}\n")),
    );
    responses.insert(ACTIVE_SLOTS_PATH.to_string(), success(""));
    responses.insert(inspect_status_command("podman", candidate_name), failure());
    responses.insert(
        image_inspect_command("podman", "example/web:latest"),
        success(""),
    );
    responses.insert(
        readiness_health_command("podman", candidate_name),
        success(""),
    );
    responses.insert(
        MKTEMP_COMMAND.to_string(),
        success("/etc/jiji/network/service-nat-generations/cutover.ghi789\n"),
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path, "podman");

    let output = run_jiji_deploy(&config_path, &[]);
    assert!(
        output.status.success(),
        "expected success, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let received = harness.received.lock().unwrap().clone();
    assert!(received.iter().any(|c| c.starts_with("podman run")));
    assert!(!received.iter().any(|c| c.starts_with("docker")));
}
