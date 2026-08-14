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

fn write_config(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("deploy.yml");
    std::fs::write(
        &path,
        r#"
project: proxy-test
builder:
  engine: docker
servers:
  web1:
    host: 192.0.2.1
  web2:
    host: 192.0.2.2
services: {}
ssh:
  user: tester
"#,
    )
    .expect("write test config");
    path
}

#[test]
fn restart_rejects_service_filter_before_loading_configuration() {
    let output = Command::new(env!("CARGO_BIN_EXE_jiji"))
        .args(["-S", "web", "-c", "/does/not/exist", "proxy", "restart"])
        .output()
        .expect("run proxy restart");

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("does not accept -S/--services"));
}

#[test]
fn logs_rejects_service_filter_before_loading_configuration() {
    let output = Command::new(env!("CARGO_BIN_EXE_jiji"))
        .args(["-S", "web", "-c", "/does/not/exist", "proxy", "logs"])
        .output()
        .expect("run proxy logs");

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("does not accept -S/--services"));
}

#[test]
fn follow_rejects_multiple_hosts_before_connecting() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let config = write_config(dir.path());
    let output = Command::new(env!("CARGO_BIN_EXE_jiji"))
        .arg("-c")
        .arg(config)
        .args(["proxy", "logs", "--follow"])
        .output()
        .expect("run proxy logs");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("requires exactly one host"), "{stderr}");
    assert!(stderr.contains("web1, web2"), "{stderr}");
}

// --- `jiji proxy restart` against a real, in-process SSH server (mirroring
// `registry_auth_test.rs`'s `TestServer`/`CannedResponse`/stdin-capture pattern) -- exercises the
// `proxy_restart` audit entry `ensure_proxy` now writes.

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

struct ProxyHarness {
    addr: SocketAddr,
    received: Arc<Mutex<Vec<String>>>,
    received_stdin: Arc<Mutex<Vec<u8>>>,
}

async fn spawn_proxy_test_server(
    authorized_key: PublicKey,
    responses: HashMap<String, CannedResponse>,
) -> ProxyHarness {
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

    ProxyHarness {
        addr,
        received,
        received_stdin,
    }
}

/// One server, private networking disabled -- `ensure_proxy` then skips `ensure_attached`/
/// `reconcile_podman_dns_address`/`ensure_ingress_rule` entirely, leaving only
/// `upload_daemon_config` + `recreate` (pull, flock rm+run, `wait_until_running`) as the remote
/// command surface this test needs to cover.
fn config_yaml(addr: SocketAddr, key_path: &std::path::Path) -> String {
    format!(
        r#"
project: proxy-audit
builder: {{ engine: docker }}
network:
  enabled: false
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

#[tokio::test(flavor = "multi_thread")]
async fn restart_writes_a_success_audit_entry_for_the_host() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert(
        "docker inspect jiji-proxy --format '{{.State.Status}}'".to_string(),
        success("running\n"),
    );
    let harness = spawn_proxy_test_server(client_key.public_key().clone(), responses).await;
    let config_path = dir.path().join("deploy.yml");
    std::fs::write(&config_path, config_yaml(harness.addr, &key_path)).expect("write config");

    let output = Command::new(env!("CARGO_BIN_EXE_jiji"))
        .arg("-c")
        .arg(&config_path)
        .args(["proxy", "restart"])
        .output()
        .expect("run proxy restart");

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
            .any(|c| c.contains("cat >> .jiji/proxy-audit/audit.log")),
        "restart should append an audit entry: {received:?}"
    );
    let received_stdin =
        String::from_utf8_lossy(&harness.received_stdin.lock().unwrap()).into_owned();
    let json_start = received_stdin
        .find("{\"timestamp\"")
        .expect("no audit JSON object in stdin");
    let audit_line = received_stdin[json_start..]
        .lines()
        .next()
        .expect("audit JSON object has at least one line");
    assert!(
        audit_line.contains("\"action\":\"proxy_restart\""),
        "{audit_line}"
    );
    assert!(
        audit_line.contains("\"status\":\"success\""),
        "{audit_line}"
    );
    // No `HostGlobalProxy` lock is taken for this command today (a pre-existing gap, not
    // introduced by the audit work), so the entry should carry no lock_scope.
    assert!(!audit_line.contains("lock_scope"), "{audit_line}");
}
