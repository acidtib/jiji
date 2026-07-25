//! Integration tests for `jiji deploy`, run as a real subprocess against a real, in-process SSH
//! server (mirroring `server_setup_test.rs`'s pattern), so the full config-load -> plan ->
//! connect -> per-endpoint deploy transaction path is exercised without touching real hosts.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
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
use tokio::io::{AsyncReadExt, AsyncWriteExt};
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
    forwards: Arc<Mutex<Vec<(String, u32)>>>,
    cancelled_forwards: Arc<Mutex<Vec<(String, u32)>>>,
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

    async fn tcpip_forward(
        &mut self,
        address: &str,
        port: &mut u32,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        self.forwards
            .lock()
            .expect("forwards mutex poisoned")
            .push((address.to_string(), *port));
        Ok(true)
    }

    async fn cancel_tcpip_forward(
        &mut self,
        address: &str,
        port: u32,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        self.cancelled_forwards
            .lock()
            .expect("cancelled forwards mutex poisoned")
            .push((address.to_string(), port));
        Ok(true)
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
    forwards: Arc<Mutex<Vec<(String, u32)>>>,
    cancelled_forwards: Arc<Mutex<Vec<(String, u32)>>>,
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
    let forwards = Arc::new(Mutex::new(Vec::new()));
    let cancelled_forwards = Arc::new(Mutex::new(Vec::new()));
    let mut test_server = TestServer {
        authorized_key,
        responses: Arc::new(responses),
        pending: Arc::new(Mutex::new(HashMap::new())),
        received: received.clone(),
        forwards: forwards.clone(),
        cancelled_forwards: cancelled_forwards.clone(),
    };

    tokio::spawn(async move {
        let _ = test_server.run_on_socket(config, &listener).await;
        drop(listener);
    });

    Harness {
        addr,
        received,
        forwards,
        cancelled_forwards,
    }
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

/// Always passes `--yes`: the test subprocess has no controlling terminal, so without it every
/// deploy would bail on the new non-interactive confirmation guard before doing anything.
fn run_jiji_deploy(config_path: &std::path::Path, extra_args: &[&str]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_jiji"));
    command
        .arg("deploy")
        .arg("-c")
        .arg(config_path)
        .arg("--yes");
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

/// Every test config in this file uses `project: demo` (see `config_yaml`), so every project-scoped
/// remote path/name can be derived from that one fixed slug.
fn slug() -> String {
    jiji_network::systemd_unit_slug("demo")
}

fn network_dir() -> String {
    format!("/etc/jiji/network/{}", slug())
}

fn active_slots_path() -> String {
    format!("cat {}/service-nat-current/active-slots", network_dir())
}

fn generation_path() -> String {
    format!("cat {}/generation 2>/dev/null || true", network_dir())
}

fn mktemp_command() -> String {
    format!(
        "mktemp -d {}/service-nat-generations/cutover.XXXXXX",
        network_dir()
    )
}

fn public_key_command() -> String {
    let dir = network_dir();
    format!("test -s {dir}/public.key && cat {dir}/public.key")
}

fn capture_generations_command() -> String {
    let dir = network_dir();
    format!(
        "set -eu; if test -L {dir}/current; then readlink -f {dir}/current; else printf '%s\\n' -; fi; if test -L {dir}/dns-current; then readlink -f {dir}/dns-current; else printf '%s\\n' -; fi"
    )
}

/// `service_network::persist_state` validates that `mktemp`'s reported path actually starts with
/// this project's `service-nat-generations/` prefix, so the canned stdout for `mktemp_command()`
/// must be project-scoped too, not just its own command key.
fn cutover_generation_path(suffix: &str) -> CannedResponse {
    success(&format!(
        "{}/service-nat-generations/cutover.{suffix}\n",
        network_dir()
    ))
}

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
async fn network_generation_mismatch_triggers_reconciliation_before_container_commands() {
    let (dir, key_path, client_key) = setup_test_dir();
    let generation = plan_generation(SocketAddr::from(([127, 0, 0, 1], 0)), "docker");
    let mut responses = HashMap::new();
    responses.insert(generation_path(), success("stale-generation\n"));
    responses.insert(
        format!("{}#2", generation_path()),
        success(&format!("{generation}\n")),
    );
    responses.insert("id -u".to_string(), success("0\n"));
    responses.insert(public_key_command(), success("test-wireguard-public-key\n"));
    responses.insert(capture_generations_command(), success("-\n-\n"));
    responses.insert(inspect_status_command("docker", "demo-web-a"), failure());
    responses.insert(
        image_inspect_command("docker", "example/web:latest"),
        success(""),
    );
    responses.insert(
        readiness_health_command("docker", "demo-web-a"),
        success(""),
    );
    responses.insert(mktemp_command(), cutover_generation_path("auto123"));

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path, "docker");

    let output = run_jiji_deploy(&config_path, &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        stdout.contains("Network topology changed"),
        "stdout: {stdout}"
    );

    let received = harness.received.lock().unwrap().clone();
    assert!(received.contains(&generation_path()));
    let activation = received
        .iter()
        .position(|command| {
            command.contains(&format!("systemctl restart jiji-dns-{}.service", slug()))
        })
        .expect("network generation should be activated");
    let container = received
        .iter()
        .position(|command| command.contains("run --name"))
        .expect("container should be created");
    assert!(
        activation < container,
        "network must activate first: {received:?}"
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
    responses.insert(generation_path(), success(&format!("{generation}\n")));
    responses.insert(active_slots_path(), success(""));
    responses.insert(inspect_status_command("docker", candidate_name), failure());
    responses.insert(
        image_inspect_command("docker", "example/web:latest"),
        success(""),
    );
    responses.insert(
        readiness_health_command("docker", candidate_name),
        success(""),
    );
    responses.insert(mktemp_command(), cutover_generation_path("abc123"));

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
    assert!(
        received
            .iter()
            .any(|c| c.contains("cat >> .jiji/demo/audit.log")
                && c.contains("install -d -m 0700 .jiji/demo")),
        "a successful deploy should append an audit entry: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn yes_flag_prints_the_deployment_plan_and_proceeds_without_prompting() {
    let (dir, key_path, client_key) = setup_test_dir();
    let generation = plan_generation(SocketAddr::from(([127, 0, 0, 1], 0)), "docker");

    let candidate_name = "demo-web-a";
    let mut responses = HashMap::new();
    responses.insert(generation_path(), success(&format!("{generation}\n")));
    responses.insert(active_slots_path(), success(""));
    responses.insert(inspect_status_command("docker", candidate_name), failure());
    responses.insert(
        image_inspect_command("docker", "example/web:latest"),
        success(""),
    );
    responses.insert(
        readiness_health_command("docker", candidate_name),
        success(""),
    );
    responses.insert(mktemp_command(), cutover_generation_path("plan123"));

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path, "docker");

    let output = run_jiji_deploy(&config_path, &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "expected success, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("Deployment Plan:"), "stdout: {stdout}");
    assert!(stdout.contains("Project: demo"), "stdout: {stdout}");
    assert!(stdout.contains("Servers: app"), "stdout: {stdout}");
    assert!(stdout.contains("Endpoints (1):"), "stdout: {stdout}");
    assert!(stdout.contains("web @ app"), "stdout: {stdout}");
    assert!(
        stdout.contains("Build: no, using configured image"),
        "stdout: {stdout}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn without_yes_and_no_terminal_deploy_refuses_to_hang_on_a_prompt() {
    let (dir, key_path, client_key) = setup_test_dir();
    let harness = spawn_test_server(client_key.public_key().clone(), HashMap::new()).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path, "docker");

    let mut command = Command::new(env!("CARGO_BIN_EXE_jiji"));
    let output = command
        .arg("deploy")
        .arg("-c")
        .arg(&config_path)
        .output()
        .expect("run jiji deploy");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success(), "stdout: {stdout}");
    assert!(stdout.contains("Deployment Plan:"), "stdout: {stdout}");
    assert!(
        stderr.contains("--yes") && stderr.contains("non-interactively"),
        "stderr: {stderr}"
    );

    // Refused before ever connecting: the SSH server received nothing at all.
    assert!(harness.received.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn replacement_removes_the_old_container_only_after_health_and_commit_succeed() {
    let (dir, key_path, client_key) = setup_test_dir();
    let generation = plan_generation(SocketAddr::from(([127, 0, 0, 1], 0)), "docker");

    let old_name = "demo-web-a";
    let candidate_name = "demo-web-b";
    let mut responses = HashMap::new();
    responses.insert(generation_path(), success(&format!("{generation}\n")));
    responses.insert(active_slots_path(), success("demo:web:app=a\n"));
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
    responses.insert(mktemp_command(), cutover_generation_path("def456"));

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
    responses.insert(generation_path(), success(&format!("{generation}\n")));
    responses.insert(active_slots_path(), success("demo:web:app=a\n"));
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
    responses.insert(generation_path(), success(&format!("{generation}\n")));
    responses.insert(active_slots_path(), success(""));
    responses.insert(inspect_status_command("podman", candidate_name), failure());
    responses.insert(
        image_inspect_command("podman", "example/web:latest"),
        success(""),
    );
    responses.insert(
        readiness_health_command("podman", candidate_name),
        success(""),
    );
    responses.insert(mktemp_command(), cutover_generation_path("ghi789"));

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

async fn run_local_registry_deploy(
    pull_succeeds: bool,
) -> (
    std::process::Output,
    u16,
    Vec<(String, u32)>,
    Vec<(String, u32)>,
    Vec<String>,
) {
    let (dir, key_path, client_key) = setup_test_dir();
    let registry_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind registry");
    let registry_port = registry_listener
        .local_addr()
        .expect("registry address")
        .port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = registry_listener.accept().await {
            tokio::spawn(async move {
                let mut request = [0_u8; 256];
                let _ = stream.read(&mut request).await;
                let _ = stream
                    .write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 2\r\n\r\n{}")
                    .await;
            });
        }
    });

    let fake_bin = dir.path().join("bin");
    std::fs::create_dir(&fake_bin).expect("create fake bin");
    let docker = fake_bin.join("docker");
    std::fs::write(
        &docker,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then exit 0; fi\nif [ \"$1\" = \"container\" ] && [ \"$2\" = \"inspect\" ]; then printf 'true|registry|{registry_port}|true\\n'; exit 0; fi\nexit 0\n"
        ),
    )
    .expect("write fake docker");
    let mut permissions = std::fs::metadata(&docker)
        .expect("docker metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&docker, permissions).expect("make fake docker executable");

    let config = format!(
        r#"
project: demo
builder:
  engine: docker
  registry:
    type: local
    port: {registry_port}
servers:
  app:
    host: {host}
    port: {ssh_port}
    keys: [{key_path}]
services:
  web:
    build: .
    hosts: [app]
ssh:
  user: tester
  keys_only: true
"#,
        host = "127.0.0.1",
        ssh_port = 0,
        key_path = key_path.display(),
    );
    let generation_config: Config = serde_yaml::from_str(&config).expect("parse generation config");
    let generation = NetworkPlanner::new()
        .plan(&generation_config)
        .expect("network plan")
        .generation;

    let mut responses = HashMap::new();
    responses.insert(generation_path(), success(&format!("{generation}\n")));
    responses.insert(active_slots_path(), success(""));
    responses.insert(inspect_status_command("docker", "demo-web-a"), failure());
    responses.insert(
        readiness_health_command("docker", "demo-web-a"),
        success(""),
    );
    responses.insert(mktemp_command(), cutover_generation_path("local123"));
    if !pull_succeeds {
        responses.insert(
            format!("docker pull localhost:{registry_port}/demo-web:v1"),
            failure(),
        );
    }
    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;

    let config = config.replace("port: 0", &format!("port: {}", harness.addr.port()));
    let config_path = dir.path().join("deploy-local.yml");
    std::fs::write(&config_path, config).expect("write local registry config");

    let existing_path = std::env::var_os("PATH").unwrap_or_default();
    let mut command = Command::new(env!("CARGO_BIN_EXE_jiji"));
    let output = command
        .arg("deploy")
        .arg("-c")
        .arg(&config_path)
        .arg("--yes")
        .arg("--build")
        .arg("--version")
        .arg("v1")
        .env(
            "PATH",
            std::env::join_paths(
                std::iter::once(fake_bin.as_path()).chain(
                    std::env::split_paths(&existing_path)
                        .collect::<Vec<_>>()
                        .iter()
                        .map(std::path::PathBuf::as_path),
                ),
            )
            .expect("join PATH"),
        )
        .output()
        .expect("run local registry deploy");

    let forwards = harness
        .forwards
        .lock()
        .expect("forwards mutex poisoned")
        .clone();
    let cancelled = harness
        .cancelled_forwards
        .lock()
        .expect("cancelled forwards mutex poisoned")
        .clone();
    let received = harness
        .received
        .lock()
        .expect("received mutex poisoned")
        .clone();
    (output, registry_port, forwards, cancelled, received)
}

#[tokio::test(flavor = "multi_thread")]
async fn local_registry_build_opens_a_loopback_reverse_tunnel_before_deploy() {
    let (output, registry_port, forwards, cancelled, received) =
        run_local_registry_deploy(true).await;
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        forwards,
        vec![("127.0.0.1".to_string(), u32::from(registry_port))]
    );
    assert_eq!(cancelled, forwards);
    assert!(received
        .iter()
        .any(|command| { command.contains(&format!("localhost:{registry_port}/demo-web:v1")) }));
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_pull_after_tunnel_setup_cancels_the_forward_and_stops_deploy() {
    let (output, registry_port, forwards, cancelled, received) =
        run_local_registry_deploy(false).await;
    assert!(!output.status.success());
    assert_eq!(
        forwards,
        vec![("127.0.0.1".to_string(), u32::from(registry_port))]
    );
    assert_eq!(cancelled, forwards);
    assert!(!received
        .iter()
        .any(|command| command.contains("run --name")));
}

#[tokio::test(flavor = "multi_thread")]
async fn deploy_bails_when_deployment_lock_is_held() {
    let (dir, key_path, client_key) = setup_test_dir();
    let generation = plan_generation(SocketAddr::from(([127, 0, 0, 1], 0)), "docker");

    let lock_path = "cat .jiji/demo/deploy.lock 2>/dev/null || true";
    let mut responses = HashMap::new();
    responses.insert(generation_path(), success(&format!("{generation}\n")));
    responses.insert(
        lock_path.to_string(),
        success(
            r#"{"message":"Deploying v1.2.3","acquired_at":1000000000,"acquired_by":"alice","pid":123}"#,
        ),
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path, "docker");

    let output = run_jiji_deploy(&config_path, &[]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success());
    assert!(
        stderr.contains("Deployment lock is held"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("Deploying v1.2.3"), "stderr: {stderr}");
    assert!(stderr.contains("alice"), "stderr: {stderr}");

    let received = harness.received.lock().unwrap().clone();
    assert!(
        !received.iter().any(|c| c.contains("run --name")),
        "no container should be touched while the lock is held: {received:?}"
    );
}
