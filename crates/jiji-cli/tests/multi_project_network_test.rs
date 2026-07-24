//! Integration test proving the actual point of the per-project network isolation work: two
//! independent projects (different `project:` names, different config files) can share one
//! physical host without one's `jiji network setup`/`jiji server teardown` touching the other's
//! paths, unit names, or bridge -- and, specifically, that a second project's preflight check
//! does not reject on seeing the first project's already-existing (non-colliding) bridge, which
//! was the critical bug this whole change exists to fix (see the project's network-isolation
//! design notes).
//!
//! Uses the same in-process russh `TestServer`/`CannedResponse`/occurrence-keyed harness pattern
//! as `deploy_test.rs`.

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

#[derive(Clone)]
struct TestServer {
    authorized_keys: Vec<PublicKey>,
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
        Ok(if self.authorized_keys.contains(key) {
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

        // `capture_installed_generation`'s combined `if test -L ...` command needs exactly two
        // lines of output (see `network::setup::parse_installed_generation`) regardless of which
        // project's slug it's checking -- a plain default-success empty response would otherwise
        // break every setup run, not just this test (mirrors `server_setup_test.rs`'s identical
        // special case).
        let response = if command.contains("if test -L ") && command.contains("/current") {
            success("-\n-\n")
        } else {
            self.responses
                .get(&format!("{command}#{occurrence}"))
                .or_else(|| self.responses.get(&command))
                .cloned()
                .unwrap_or_else(|| success(""))
        };

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
    authorized_keys: Vec<PublicKey>,
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
        authorized_keys,
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

/// A single server ("app") on a single project, with no services -- keeps the run focused on the
/// network layer, not application deployment.
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

fn write_config(dir: &std::path::Path, name: &str, contents: &str) -> std::path::PathBuf {
    let config_path = dir.join(name);
    std::fs::write(&config_path, contents).expect("write test config");
    config_path
}

fn plan_for(project: &str, addr: SocketAddr) -> jiji_network::NetworkPlan {
    let yaml = config_yaml(project, addr, std::path::Path::new("/dev/null"));
    let config: Config = serde_yaml::from_str(&yaml).expect("parse test config");
    NetworkPlanner::new()
        .plan(&config)
        .expect("build test plan")
}

fn run_jiji_network_setup(config_path: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_jiji"))
        .arg("network")
        .arg("setup")
        .arg("-c")
        .arg(config_path)
        .output()
        .expect("run jiji network setup")
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

const PREFLIGHT_COMMAND: &str = "ip -o -4 route show table all; if command -v docker >/dev/null 2>&1; then docker network inspect $(docker network ls -q) --format 'NETWORK {{.Name}} {{range .IPAM.Config}}{{.Subnet}} {{end}}' 2>/dev/null || true; fi; \
         wg show all listen-port 2>/dev/null | sed 's/^/PORT /' || true; \
         ip -o -4 address show | sed 's/^/ADDR /'";

fn public_key_response(project: &str, key: &str) -> (String, CannedResponse) {
    let slug = jiji_network::systemd_unit_slug(project);
    let dir = format!("/etc/jiji/network/{slug}");
    (
        format!("test -s {dir}/public.key && cat {dir}/public.key"),
        success(&format!("{key}\n")),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_project_coexists_with_the_first_on_one_host() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let (key_path_a, client_key_a) = setup_key(dir.path(), "id_a");
    let (key_path_b, client_key_b) = setup_key(dir.path(), "id_b");

    let plan_a = plan_for("project-a", SocketAddr::from(([127, 0, 0, 1], 0)));
    let server_a = &plan_a.servers["app"];
    let slug_a = jiji_network::systemd_unit_slug("project-a");
    let slug_b = jiji_network::systemd_unit_slug("project-b");

    let mut responses = HashMap::new();
    responses.insert("id -u".to_string(), success("0\n"));
    let (key_a, resp_a) = public_key_response("project-a", "project-a-public-key");
    responses.insert(key_a, resp_a);
    let (key_b, resp_b) = public_key_response("project-b", "project-b-public-key");
    responses.insert(key_b, resp_b);
    // Occurrence 1: project A's own preflight, host has no existing jiji bridges yet.
    responses.insert(format!("{PREFLIGHT_COMMAND}#1"), success(""));
    // Occurrence 2: project B's preflight, host already has project A's bridge -- non-colliding
    // subnet, so this must be *allowed*, not rejected (the critical fix this test exists for).
    responses.insert(
        format!("{PREFLIGHT_COMMAND}#2"),
        success(&format!(
            "NETWORK {} {} \n",
            server_a.bridge_name, server_a.container_subnet
        )),
    );

    let harness = spawn_test_server(
        vec![
            client_key_a.public_key().clone(),
            client_key_b.public_key().clone(),
        ],
        responses,
    )
    .await;

    let config_a = write_config(
        dir.path(),
        "a.yml",
        &config_yaml("project-a", harness.addr, &key_path_a),
    );
    let config_b = write_config(
        dir.path(),
        "b.yml",
        &config_yaml("project-b", harness.addr, &key_path_b),
    );

    let output_a = run_jiji_network_setup(&config_a);
    assert!(
        output_a.status.success(),
        "project A setup should succeed, stderr: {}",
        String::from_utf8_lossy(&output_a.stderr)
    );

    let marker = harness.received.lock().unwrap().len();

    let output_b = run_jiji_network_setup(&config_b);
    assert!(
        output_b.status.success(),
        "project B setup must succeed even though project A's bridge already exists on this \
         host, stderr: {}",
        String::from_utf8_lossy(&output_b.stderr)
    );

    let received = harness.received.lock().unwrap().clone();
    let (a_commands, b_commands) = received.split_at(marker);

    assert!(
        !b_commands.iter().any(|c| c.contains(&slug_a)),
        "project B must never send a command referencing project A's slug ({slug_a}): {b_commands:?}"
    );
    assert!(
        !a_commands.iter().any(|c| c.contains(&slug_b)),
        "project A must never send a command referencing project B's slug ({slug_b}): {a_commands:?}"
    );
    assert!(
        b_commands
            .iter()
            .any(|c| c.contains(&format!("/etc/jiji/network/{slug_b}"))),
        "project B should still stage its own project-scoped state: {b_commands:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn tearing_down_one_project_never_references_a_sibling_projects_slug() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let (key_path, client_key) = setup_key(dir.path(), "id");

    let slug_a = jiji_network::systemd_unit_slug("project-a");
    let slug_b = jiji_network::systemd_unit_slug("project-b");

    // No canned responses needed: every unmatched command defaults to a bare success (empty
    // output), which already gives an empty container list, an empty proxy-route list, and so
    // on -- exactly the "nothing here" state this test wants.
    let responses = HashMap::new();

    let harness = spawn_test_server(vec![client_key.public_key().clone()], responses).await;
    let config_path = write_config(
        dir.path(),
        "a.yml",
        &config_yaml("project-a", harness.addr, &key_path),
    );

    let _output = Command::new(env!("CARGO_BIN_EXE_jiji"))
        .arg("server")
        .arg("teardown")
        .arg("-y")
        .arg("-c")
        .arg(&config_path)
        .output()
        .expect("run jiji server teardown");

    // Not asserting overall success here -- the mock host has no real containers/network state
    // to discover, so several steps may report "already absent"/"failed" for reasons unrelated to
    // this test. What matters is which project's names ever appear on the wire.
    let received = harness.received.lock().unwrap().clone();
    assert!(
        !received.iter().any(|c| c.contains(&slug_b)),
        "tearing down project A must never reference project B's slug ({slug_b}): {received:?}"
    );
    assert!(
        received.iter().any(|c| c.contains(&slug_a)),
        "tearing down project A should still reference its own slug ({slug_a}): {received:?}"
    );
}
