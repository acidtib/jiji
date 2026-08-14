//! Integration tests for `jiji audit` and its write side (wired into `jiji lock acquire`/
//! `release` and `jiji deploy`), run as a real subprocess against a real, in-process SSH server
//! (mirroring `lock_commands_test.rs`'s pattern). The audit trail is host-scoped, like locks, so
//! there is no network-generation reconciliation or endpoint selection involved for the read side.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
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
    stdin: Arc<Mutex<HashMap<ChannelId, Vec<u8>>>>,
    received: Arc<Mutex<Vec<String>>>,
    /// Every channel's stdin bytes, flattened in close order (no per-command separator) -- enough
    /// to find and parse a piped JSON audit line via a `{"timestamp"` scan, which the
    /// `*_writes_an_audit_entry` tests below rely on.
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
            .or_else(|| {
                self.responses.iter().find_map(|(pattern, response)| {
                    pattern
                        .strip_prefix("PREFIX:")
                        .filter(|prefix| command.starts_with(prefix))
                        .map(|_| response)
                })
            })
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
    received_stdin: Arc<Mutex<Vec<u8>>>,
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

    Harness {
        addr,
        received,
        received_stdin,
    }
}

/// Finds the first JSON audit object piped over stdin and returns its own line, isolated from
/// whatever unrelated stdin bytes (e.g. another command's piped content, with no trailing
/// newline of its own) happen to precede it in this flat, per-session capture buffer.
fn find_audit_json_line(received_stdin: &[u8]) -> String {
    let text = String::from_utf8_lossy(received_stdin).into_owned();
    let json_start = text
        .find("{\"timestamp\"")
        .unwrap_or_else(|| panic!("no audit JSON object in stdin: {text:?}"));
    text[json_start..]
        .lines()
        .next()
        .expect("audit JSON object has at least one line")
        .to_string()
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
const AUDIT_READ_ALL: &str = "cat .jiji/demo/audit.log 2>/dev/null || true";
const LOCK_PATH: &str = "cat .jiji/demo/locks/maintenance.lock/info.json 2>/dev/null || true";

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
async fn audit_stats_aggregates_all_entries_by_action_and_server() {
    let (dir, key_path, client_key) = setup_test_dir();
    let body = "{\"timestamp\":4102444800,\"action\":\"deploy\",\"status\":\"success\",\"actor\":\"tester\",\"message\":\"ok\",\"duration_ms\":1000}\n\
                {\"timestamp\":4102444801,\"action\":\"deploy\",\"status\":\"failed\",\"actor\":\"tester\",\"message\":\"failed\",\"duration_ms\":3000}\n\
                {\"timestamp\":4102444802,\"action\":\"lock_acquire\",\"status\":\"success\",\"actor\":\"tester\",\"message\":\"locked\"}\n";
    let mut responses = HashMap::new();
    responses.insert(AUDIT_READ_ALL.to_string(), success(body));

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);

    let output = run_jiji_audit(&config_path, &["--stats"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("all: 3 entries, 2 success, 1 failed, 66.7% success"));
    assert!(stdout.contains("deploy: 2 entries, 1 success, 1 failed, 50.0% success, avg 2.0s"));
    assert!(stdout.contains("app: 3 entries, 2 success, 1 failed"));
}

#[tokio::test(flavor = "multi_thread")]
async fn audit_stats_since_filters_entries_and_json_is_structured() {
    let (dir, key_path, client_key) = setup_test_dir();
    let body = "{\"timestamp\":1,\"action\":\"deploy\",\"status\":\"failed\",\"actor\":\"tester\",\"message\":\"old\",\"duration_ms\":9000}\n\
                {\"timestamp\":4102444800,\"action\":\"deploy\",\"status\":\"success\",\"actor\":\"tester\",\"message\":\"recent\",\"duration_ms\":1000}\n";
    let mut responses = HashMap::new();
    responses.insert("PREFIX:awk -v cutoff=".to_string(), success(body));

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);

    let output = run_jiji_audit(&config_path, &["--stats", "--since", "1h", "--json"]);
    assert!(output.status.success());
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("valid stats json");
    assert_eq!(parsed["overall"]["entries"], 1);
    assert_eq!(parsed["overall"]["successes"], 1);
    assert_eq!(parsed["by_action"]["deploy"]["average_duration_ms"], 1000);
    assert_eq!(parsed["by_server"]["app"]["entries"], 1);
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

/// One server ("app"), used by the `network compact` audit test below.
fn config_yaml_for_project(project: &str, addr: SocketAddr, key_path: &std::path::Path) -> String {
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

#[tokio::test(flavor = "multi_thread")]
async fn network_compact_writes_a_success_audit_entry_per_host() {
    let (dir, key_path, client_key) = setup_test_dir();
    let project = "compact-audit";
    let paths = AgentPaths::default_for_project(project);
    let request_command = format!(
        "{} request --socket {} # jiji-request:compact",
        paths.binary_path.display(),
        paths.socket_path.display()
    );

    let mut responses = HashMap::new();
    responses.insert(
        format!("cat .jiji/{project}/locks/maintenance.lock/info.json 2>/dev/null || true"),
        success(""),
    );
    responses.insert(
        request_command,
        success(r#"{"Ok":{"type":"compacted","membership_removed":1,"catalog_removed":2,"desired_removed":3}}"#),
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = dir.path().join("deploy.yml");
    std::fs::write(
        &config_path,
        config_yaml_for_project(project, harness.addr, &key_path),
    )
    .expect("write test config");

    let output = Command::new(env!("CARGO_BIN_EXE_jiji"))
        .arg("-c")
        .arg(&config_path)
        .args(["network", "compact"])
        .output()
        .expect("run jiji network compact");

    assert!(
        output.status.success(),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let received = harness.received.lock().unwrap().clone();
    assert!(
        received
            .iter()
            .any(|c| c.contains(&format!("cat >> .jiji/{project}/audit.log"))),
        "compact should append an audit entry: {received:?}"
    );

    let received_stdin = harness.received_stdin.lock().unwrap().clone();
    let audit_line = find_audit_json_line(&received_stdin);
    assert!(
        audit_line.contains("\"action\":\"network_compact\""),
        "{audit_line}"
    );
    assert!(
        audit_line.contains("\"status\":\"success\""),
        "{audit_line}"
    );
    assert!(
        audit_line.contains("\"lock_scope\":\"project-maintenance\""),
        "{audit_line}"
    );
    assert!(
        audit_line.contains("removed 1 membership, 2 catalog, 3 desired operation(s)"),
        "{audit_line}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn network_restore_writes_a_success_audit_entry_per_host() {
    let (dir, key_path, client_key) = setup_test_dir();
    let project = "restore-audit";
    let paths = AgentPaths::default_for_project(project);

    let export_command = format!(
        "{} backup-export --project {project} --state-dir {} --mesh-config {}",
        paths.binary_path.display(),
        paths.state_dir.display(),
        paths.mesh_config_path.display(),
    );
    let snapshot_json = format!(
        r#"{{"format_version":2,"project_id":"{project}","recovery_epoch":1,"node_id":"app","catalog":[],"desired":[],"address_leases":[]}}"#
    );
    let remote_input = paths.state_dir.join("restore-input.json");
    let restore_command = format!(
        "install -m 0600 /dev/stdin {input}; \
             {binary} backup-import --project {project} --state-dir {state} \
             --mesh-config {mesh} --input {input}; code=$?; rm -f {input}; exit $code",
        input = remote_input.display(),
        binary = paths.binary_path.display(),
        state = paths.state_dir.display(),
        mesh = paths.mesh_config_path.display(),
    );

    let mut responses = HashMap::new();
    responses.insert(
        format!("cat .jiji/{project}/locks/maintenance.lock/info.json 2>/dev/null || true"),
        success(""),
    );
    responses.insert(export_command, success(&snapshot_json));
    responses.insert(restore_command, success(""));

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = dir.path().join("deploy.yml");
    std::fs::write(
        &config_path,
        config_yaml_for_project(project, harness.addr, &key_path),
    )
    .expect("write test config");

    let passphrase_path = dir.path().join("passphrase.txt");
    std::fs::write(&passphrase_path, "correct horse battery staple").expect("write passphrase");
    std::fs::set_permissions(&passphrase_path, std::fs::Permissions::from_mode(0o600))
        .expect("chmod passphrase file");

    let backup_path = dir.path().join("backup.enc");
    let backup_output = Command::new(env!("CARGO_BIN_EXE_jiji"))
        .arg("-c")
        .arg(&config_path)
        .arg("network")
        .arg("backup")
        .arg("--output")
        .arg(&backup_path)
        .arg("--passphrase-file")
        .arg(&passphrase_path)
        .output()
        .expect("run jiji network backup");
    assert!(
        backup_output.status.success(),
        "backup export should succeed, stdout: {} stderr: {}",
        String::from_utf8_lossy(&backup_output.stdout),
        String::from_utf8_lossy(&backup_output.stderr)
    );

    let restore_output = Command::new(env!("CARGO_BIN_EXE_jiji"))
        .arg("-c")
        .arg(&config_path)
        .arg("network")
        .arg("restore")
        .arg("--input")
        .arg(&backup_path)
        .arg("--passphrase-file")
        .arg(&passphrase_path)
        .output()
        .expect("run jiji network restore");
    assert!(
        restore_output.status.success(),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&restore_output.stdout),
        String::from_utf8_lossy(&restore_output.stderr)
    );

    let received = harness.received.lock().unwrap().clone();
    assert!(
        received
            .iter()
            .any(|c| c.contains(&format!("cat >> .jiji/{project}/audit.log"))),
        "restore should append an audit entry: {received:?}"
    );

    // Only `network restore` is instrumented (the export/`backup` phase above is a read, and
    // `with_project_maintenance_lock`'s internal locking writes no audit entry of its own), so
    // the first JSON object piped over stdin across this whole run is the restore's own entry.
    let received_stdin = harness.received_stdin.lock().unwrap().clone();
    let audit_line = find_audit_json_line(&received_stdin);
    assert!(
        audit_line.contains("\"action\":\"network_restore\""),
        "{audit_line}"
    );
    assert!(
        audit_line.contains("\"status\":\"success\""),
        "{audit_line}"
    );
    assert!(
        audit_line.contains("\"lock_scope\":\"project-maintenance\""),
        "{audit_line}"
    );
    assert!(
        audit_line.contains("same-epoch state restored"),
        "{audit_line}"
    );
}
