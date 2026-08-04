//! Integration tests for `jiji network assess` and `jiji network import` (Phase 8, "Clean
//! Cutover, Optional Import, and Release"). Uses the same in-process russh
//! `TestServer`/`CannedResponse` harness as `network_rollback_test.rs`; every command these two
//! commands run is a deterministic string (no content-hash-embedding activation step like
//! `network setup`), so exact-match canned responses are sufficient -- no substring matching
//! needed.

use std::collections::HashMap;
use std::net::SocketAddr;
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

fn config_yaml(project: &str, addr: SocketAddr, key_path: &std::path::Path) -> String {
    format!(
        r#"
project: {project}
builder: {{ engine: docker }}
servers:
  app:
    host: {ip}
    port: {port}
    keys:
      - {key_path}
services:
  web:
    image: nginx:alpine
    servers: [app]
ssh:
  user: tester
  keys_only: true
"#,
        ip = addr.ip(),
        port = addr.port(),
        key_path = key_path.display(),
    )
}

fn setup_key(dir: &std::path::Path, name: &str) -> (std::path::PathBuf, PrivateKey) {
    let client_key = PrivateKey::random(&mut rng(), Algorithm::Ed25519).expect("generate key");
    let key_path = dir.join(name);
    std::fs::write(
        &key_path,
        client_key
            .to_openssh(LineEnding::LF)
            .expect("encode key as openssh")
            .as_bytes(),
    )
    .expect("write key file");
    (key_path, client_key)
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
async fn assess_reports_importable_when_old_container_has_no_catalog_record() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let (key_path, client_key) = setup_key(dir.path(), "id");
    let project = "assess-demo";
    let paths = AgentPaths::default_for_project(project);

    let mut responses = HashMap::new();
    responses.insert(
        container_list_command(project),
        success("assess-demo-web-a|assess-demo|web|app|running\n"),
    );
    responses.insert(
        format!(
            "{} catalog-export --state-dir {} 2>/dev/null || true",
            paths.binary_path.display(),
            paths.state_dir.display()
        ),
        success(""),
    );
    responses.insert(
        format!(
            "{} membership-export --state-dir {} 2>/dev/null || true",
            paths.binary_path.display(),
            paths.state_dir.display()
        ),
        success(""),
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = dir.path().join("deploy.yml");
    std::fs::write(&config_path, config_yaml(project, harness.addr, &key_path))
        .expect("write test config");

    let output = Command::new(env!("CARGO_BIN_EXE_jiji"))
        .arg("network")
        .arg("assess")
        .arg("-c")
        .arg(&config_path)
        .output()
        .expect("run jiji network assess");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "assess should succeed on advisory findings, stdout: {stdout} stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("importable -- web"),
        "expected an importable finding for web, stdout: {stdout}"
    );
    assert!(
        !stdout.contains("already migrated"),
        "web has no catalog record yet, so it must not be reported as migrated: {stdout}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn assess_reports_already_migrated_when_a_catalog_record_exists() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let (key_path, client_key) = setup_key(dir.path(), "id");
    let project = "assess-migrated";
    let paths = AgentPaths::default_for_project(project);
    let replica_id = jiji_cli::placement::replica_id(project, "web", 0);

    let mut responses = HashMap::new();
    responses.insert(
        container_list_command(project),
        success("assess-migrated-web-a|assess-migrated|web|app|running\n"),
    );
    responses.insert(
        format!(
            "{} catalog-export --state-dir {} 2>/dev/null || true",
            paths.binary_path.display(),
            paths.state_dir.display()
        ),
        success(&format!(
            r#"[{{"project_id":"assess-migrated","recovery_epoch":1,"protocol_version":1,"schema_version":2,"service":"web","replica_id":"{replica_id}","owner_node_id":"node-a","owner_epoch":1,"revision":1,"deployment_id":"deploy-1","address":"100.64.0.10","ports":[],"image":"nginx:alpine","state":"active","health":"healthy"}}]"#
        )),
    );
    responses.insert(
        format!(
            "{} membership-export --state-dir {} 2>/dev/null || true",
            paths.binary_path.display(),
            paths.state_dir.display()
        ),
        success(""),
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = dir.path().join("deploy.yml");
    std::fs::write(&config_path, config_yaml(project, harness.addr, &key_path))
        .expect("write test config");

    let output = Command::new(env!("CARGO_BIN_EXE_jiji"))
        .arg("network")
        .arg("assess")
        .arg("-c")
        .arg(&config_path)
        .output()
        .expect("run jiji network assess");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "assess should succeed: {stdout}");
    assert!(
        stdout.contains("already migrated -- web"),
        "expected web to be reported as already migrated, stdout: {stdout}"
    );
    assert!(
        !stdout.contains("importable"),
        "web already has a catalog record, so it must not be reported as importable: {stdout}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn import_dry_run_reports_the_plan_without_committing_anything() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let (key_path, client_key) = setup_key(dir.path(), "id");
    let project = "import-dry-run";
    let paths = AgentPaths::default_for_project(project);

    let mut responses = HashMap::new();
    responses.insert(
        container_list_command(project),
        success("import-dry-run-web-a|import-dry-run|web|app|running\n"),
    );
    responses.insert(
        agent_request_command(&paths, "catalog-list"),
        success(r#"{"Ok":{"type":"catalog_list","records":[]}}"#),
    );
    responses.insert(
        "docker inspect import-dry-run-web-a --format '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' 2>/dev/null || true".to_string(),
        success("100.64.0.10\n"),
    );
    responses.insert(
        "docker inspect import-dry-run-web-a --format '{{.Config.Image}}' 2>/dev/null || true"
            .to_string(),
        success("nginx:alpine\n"),
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = dir.path().join("deploy.yml");
    std::fs::write(&config_path, config_yaml(project, harness.addr, &key_path))
        .expect("write test config");

    let output = Command::new(env!("CARGO_BIN_EXE_jiji"))
        .arg("network")
        .arg("import")
        .arg("--dry-run")
        .arg("-c")
        .arg(&config_path)
        .output()
        .expect("run jiji network import");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "dry-run import should succeed, stdout: {stdout} stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("web -> replica"),
        "expected the dry-run plan to list the web replica, stdout: {stdout}"
    );

    let received = harness.received.lock().unwrap().clone();
    assert!(
        !received
            .iter()
            .any(|command| command.contains("# jiji-request:catalog-commit")),
        "dry-run must never commit anything: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn import_skips_a_replica_that_already_has_a_live_catalog_record() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let (key_path, client_key) = setup_key(dir.path(), "id");
    let project = "import-live-guard";
    let paths = AgentPaths::default_for_project(project);
    let replica_id = jiji_cli::placement::replica_id(project, "web", 0);

    let mut responses = HashMap::new();
    responses.insert(
        container_list_command(project),
        success("import-live-guard-web-a|import-live-guard|web|app|running\n"),
    );
    responses.insert(
        agent_request_command(&paths, "catalog-list"),
        success(&format!(
            r#"{{"Ok":{{"type":"catalog_list","records":[{{"project_id":"import-live-guard","recovery_epoch":1,"protocol_version":1,"schema_version":2,"service":"web","replica_id":"{replica_id}","owner_node_id":"node-a","owner_epoch":1,"revision":3,"deployment_id":"deploy-live","address":"100.64.0.20","ports":[],"image":"nginx:alpine","state":"active","health":"healthy"}}]}}}}"#
        )),
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = dir.path().join("deploy.yml");
    std::fs::write(&config_path, config_yaml(project, harness.addr, &key_path))
        .expect("write test config");

    let output = Command::new(env!("CARGO_BIN_EXE_jiji"))
        .arg("network")
        .arg("import")
        .arg("--dry-run")
        .arg("-c")
        .arg(&config_path)
        .output()
        .expect("run jiji network import");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "dry-run import should succeed even with nothing to import: {stdout}"
    );
    assert!(
        stdout.contains("Nothing to import"),
        "an already-live replica must never be re-imported: {stdout}"
    );
}
