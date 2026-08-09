//! Integration tests for `jiji service cron list/status/logs/run`
//! (`docs/architecture-notes.md#scheduled-cron-execution-crons`), run as a real subprocess
//! against a real, in-process SSH server
//! (mirroring `deploy_test.rs`'s pattern): these commands now connect to a service's current
//! owner over SSH to read/apply agent state, unlike Phase 1's purely-local/not-yet-implemented
//! stubs.

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

fn default_response(command: &str) -> CannedResponse {
    let body = if command.contains("# jiji-request:catalog-list") {
        r#"{"Ok":{"type":"catalog_list","records":[]}}"#
    } else {
        ""
    };
    success(body)
}

fn agent_request_command(kind: &str) -> String {
    format!(
        "/etc/jiji/agent/demo-354b6884/bin/jiji-agent request --socket \
         /etc/jiji/agent/demo-354b6884/agent.sock # jiji-request:{kind}"
    )
}

/// One Active/Healthy catalog record for `worker`'s single replica, owned by "app" (this file's
/// only configured server).
fn active_catalog_response_for(
    service: &str,
    replica_id: &str,
    deployment_id: &str,
) -> CannedResponse {
    success(&format!(
        r#"{{"Ok":{{"type":"catalog_list","records":[{{"project_id":"demo","recovery_epoch":1,"protocol_version":1,"schema_version":2,"service":"{service}","replica_id":"{replica_id}","owner_node_id":"app","owner_epoch":1,"revision":2,"deployment_id":"{deployment_id}","address":"100.64.0.9","ports":[],"image":"docker.io/library/worker:latest","state":"active","health":"healthy"}}]}}}}"#
    ))
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
            .unwrap_or_else(|| default_response(&command));

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

fn config_yaml(addr: SocketAddr, key_path: &std::path::Path) -> String {
    format!(
        r#"
project: demo
builder: {{ engine: podman }}
servers:
  app:
    host: {ip}
    port: {port}
    keys:
      - {key_path}
services:
  web:
    image: nginx
    servers: [app]
  worker:
    image: worker
    servers: [app]
    crons:
      sync-data:
        schedule: "0 3 * * *"
        command: ["sync"]
      cleanup:
        schedule: "30 4 * * *"
        command: ["cleanup"]
  another-worker:
    image: worker
    servers: [app]
    crons:
      backup:
        schedule: "0 5 * * *"
        command: ["backup"]
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

fn run_jiji(config_path: &std::path::Path, args: &[&str]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_jiji"));
    command.arg("service").arg("cron");
    for arg in args {
        command.arg(arg);
    }
    command.arg("-c").arg(config_path);
    command.output().expect("run jiji service cron")
}

#[test]
fn help_lists_the_four_subcommands() {
    let output = Command::new(env!("CARGO_BIN_EXE_jiji"))
        .args(["service", "cron", "--help"])
        .output()
        .expect("run jiji service cron --help");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    for expected in ["list", "status", "logs", "run"] {
        assert!(
            stdout.contains(expected),
            "stdout missing '{expected}': {stdout}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn list_reports_no_cron_jobs_for_a_service_without_any() {
    let (dir, key_path, client_key) = setup_test_dir();
    let harness = spawn_test_server(client_key.public_key().clone(), HashMap::new()).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);

    let output = run_jiji(&config_path, &["list", "-S", "web"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        stdout.contains("No cron jobs are configured"),
        "stdout: {stdout}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn list_reports_not_deployed_when_no_active_replica_exists() {
    let (dir, key_path, client_key) = setup_test_dir();
    // Default catalog-list response (empty records): no owner, so every cron is not-deployed.
    let harness = spawn_test_server(client_key.public_key().clone(), HashMap::new()).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);

    let output = run_jiji(&config_path, &["list", "-S", "worker"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        stdout.contains("sync-data") && stdout.contains("state=not-deployed"),
        "stdout: {stdout}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn list_reports_installed_and_drifted_states() {
    let (dir, key_path, client_key) = setup_test_dir();
    let replica_id = jiji_cli::placement::replica_id("demo", "worker", 0);

    // "cleanup"'s installed hash won't match anything real jiji renders (no real spec-apply ever
    // ran), so it reports drifted; "sync-data" has no installed entry at all, so not-deployed.
    let specs = format!(
        r#"{{"Ok":{{"type":"cron_specs","specs":[{{"project":"demo","service":"worker","cron_name":"cleanup","revision":1,"canonical_hash":"stale-hash","owner_node_id":"app","owner_epoch":1,"server":"app","source_deployment_id":"dep-a","source_replica_id":"{replica_id}","image":"docker.io/library/worker:latest","schedule":"30 4 * * *","timezone":"UTC","timeout_seconds":3600,"overlap":"forbid","missed_runs":"skip","command":["cleanup"],"env_file_path":".jiji/demo/env/worker-app.env","mount_args":[],"resource_args":[],"bridge_network":"jiji-demo","dns_address":"100.64.0.5"}}]}}}}"#
    );
    let mut responses = HashMap::new();
    responses.insert(
        agent_request_command("catalog-list"),
        active_catalog_response_for("worker", &replica_id, "dep-a"),
    );
    responses.insert(agent_request_command("cron-spec-list"), success(&specs));

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);

    let output = run_jiji(&config_path, &["list", "-S", "worker"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        stdout.contains("worker sync-data:") && stdout.contains("state=not-deployed"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("worker cleanup:") && stdout.contains("state=drifted"),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("owner=app"), "stdout: {stdout}");
}

#[tokio::test(flavor = "multi_thread")]
async fn status_reports_no_owner_actionably() {
    let (dir, key_path, client_key) = setup_test_dir();
    let harness = spawn_test_server(client_key.public_key().clone(), HashMap::new()).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);

    let output = run_jiji(&config_path, &["status", "-S", "worker"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no active, healthy replica"),
        "stderr: {stderr}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn status_reports_no_match_when_filter_selects_no_cron_service() {
    let (dir, key_path, client_key) = setup_test_dir();
    let harness = spawn_test_server(client_key.public_key().clone(), HashMap::new()).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);

    let output = run_jiji(&config_path, &["status", "-S", "web"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("No service with cron jobs matched"),
        "stderr: {stderr}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn status_reports_durable_state_from_the_owner() {
    let (dir, key_path, client_key) = setup_test_dir();
    let replica_id = jiji_cli::placement::replica_id("demo", "worker", 0);
    let statuses = r#"{"Ok":{"type":"cron_statuses","statuses":[{"service":"worker","cron_name":"sync-data","last_scheduled_at":1704067200,"last_started_at":1704067201,"last_finished_at":1704067210,"last_state":"succeeded","last_exit_code":0,"next_due_at":1704153600,"active_run_id":null,"skipped_overlap_count":2}]}}"#;

    let mut responses = HashMap::new();
    responses.insert(
        agent_request_command("catalog-list"),
        active_catalog_response_for("worker", &replica_id, "dep-a"),
    );
    responses.insert(agent_request_command("cron-status"), success(statuses));

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);

    let output = run_jiji(&config_path, &["status", "-S", "worker"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        stdout.contains("worker sync-data:") && stdout.contains("skipped_overlap=2"),
        "stdout: {stdout}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn logs_reports_unknown_cron_name_with_available_names() {
    let (dir, key_path, client_key) = setup_test_dir();
    let harness = spawn_test_server(client_key.public_key().clone(), HashMap::new()).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);

    let output = run_jiji(&config_path, &["logs", "nope", "-S", "worker"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("has no cron named 'nope'") && stderr.contains("cleanup, sync-data"),
        "stderr: {stderr}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn logs_rejects_a_filter_matching_multiple_cron_services() {
    let (dir, key_path, client_key) = setup_test_dir();
    let harness = spawn_test_server(client_key.public_key().clone(), HashMap::new()).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);

    let output = run_jiji(
        &config_path,
        &["logs", "backup", "-S", "worker,another-worker"],
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("matched 2 services with cron jobs"),
        "stderr: {stderr}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn logs_reports_no_owner_actionably() {
    let (dir, key_path, client_key) = setup_test_dir();
    let harness = spawn_test_server(client_key.public_key().clone(), HashMap::new()).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);

    let output = run_jiji(&config_path, &["logs", "sync-data", "-S", "worker"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no active, healthy replica"),
        "stderr: {stderr}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn logs_rejects_follow_combined_with_run_id() {
    let (dir, key_path, client_key) = setup_test_dir();
    let harness = spawn_test_server(client_key.public_key().clone(), HashMap::new()).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);

    let output = run_jiji(
        &config_path,
        &[
            "logs",
            "sync-data",
            "-S",
            "worker",
            "--run",
            "abc123",
            "--follow",
        ],
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--follow reads the active run"),
        "stderr: {stderr}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn logs_prints_the_latest_runs_output() {
    let (dir, key_path, client_key) = setup_test_dir();
    let replica_id = jiji_cli::placement::replica_id("demo", "worker", 0);
    let runs = r#"{"Ok":{"type":"cron_runs","runs":[{"run_id":"run-1","project":"demo","service":"worker","cron_name":"sync-data","cause":"scheduled","scheduled_at":1704067200,"claimed_at":1704067200,"started_at":1704067201,"finished_at":1704067210,"state":"succeeded","deployment_id":"run-1","container_name":"demo-worker-cron-sync-data-run1","address":"100.64.0.9","exit_code":0,"error":null}]}}"#;

    let mut responses = HashMap::new();
    responses.insert(
        agent_request_command("catalog-list"),
        active_catalog_response_for("worker", &replica_id, "dep-a"),
    );
    responses.insert(agent_request_command("cron-runs"), success(runs));
    responses.insert(
        "podman logs --timestamps --tail=100 demo-worker-cron-sync-data-run1".to_string(),
        success("sync complete\n"),
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);

    let output = run_jiji(&config_path, &["logs", "sync-data", "-S", "worker"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(stdout.contains("sync complete"), "stdout: {stdout}");
}

#[tokio::test(flavor = "multi_thread")]
async fn run_rejects_unknown_cron_name() {
    let (dir, key_path, client_key) = setup_test_dir();
    let harness = spawn_test_server(client_key.public_key().clone(), HashMap::new()).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);

    let output = run_jiji(&config_path, &["run", "nope", "-S", "another-worker"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("has no cron named 'nope'"),
        "stderr: {stderr}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn run_reports_no_owner_actionably() {
    let (dir, key_path, client_key) = setup_test_dir();
    let harness = spawn_test_server(client_key.public_key().clone(), HashMap::new()).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);

    let output = run_jiji(&config_path, &["run", "backup", "-S", "another-worker"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no active, healthy replica"),
        "stderr: {stderr}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn run_accepts_and_reports_the_run_id() {
    let (dir, key_path, client_key) = setup_test_dir();
    let replica_id = jiji_cli::placement::replica_id("demo", "another-worker", 0);
    let accepted = r#"{"Ok":{"type":"cron_run_accepted","run_id":"run-abc123"}}"#;

    let mut responses = HashMap::new();
    responses.insert(
        agent_request_command("catalog-list"),
        active_catalog_response_for("another-worker", &replica_id, "dep-a"),
    );
    responses.insert(agent_request_command("cron-run"), success(accepted));

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);

    let output = run_jiji(&config_path, &["run", "backup", "-S", "another-worker"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(stdout.contains("run-abc123"), "stdout: {stdout}");

    let received = harness.received.lock().unwrap().clone();
    assert!(
        received
            .iter()
            .any(|command| command.contains("# jiji-request:cron-run")),
        "the agent must have received a cron-run request: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn run_reports_a_conflict_as_an_actionable_error() {
    let (dir, key_path, client_key) = setup_test_dir();
    let replica_id = jiji_cli::placement::replica_id("demo", "another-worker", 0);
    let conflict = r#"{"Ok":{"type":"cron_run_conflict","active_run_id":"run-already-active"}}"#;

    let mut responses = HashMap::new();
    responses.insert(
        agent_request_command("catalog-list"),
        active_catalog_response_for("another-worker", &replica_id, "dep-a"),
    );
    responses.insert(agent_request_command("cron-run"), success(conflict));

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);

    let output = run_jiji(&config_path, &["run", "backup", "-S", "another-worker"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("already running") && stderr.contains("run-already-active"),
        "stderr: {stderr}"
    );
}
