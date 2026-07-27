//! Integration tests for `jiji server setup`, run as a real subprocess against a real, in-process
//! SSH server (mirroring `jiji-ssh`'s own `session_test.rs` pattern), so the full config-load ->
//! connect -> engine-check/install path is exercised without touching a real host.

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

/// Canned response for a specific command. `None` for a field falls back to the server's
/// default (success, no output).
#[derive(Clone, Default)]
struct CannedResponse {
    success: bool,
    stdout: String,
    stderr: String,
}

#[derive(Clone)]
struct TestServer {
    authorized_key: PublicKey,
    responses: HashMap<String, CannedResponse>,
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

    // Deferring to EOF avoids a race with the client's pipelined exec+eof messages, same as
    // jiji-ssh's own test server.
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

        // Commands with no canned response (e.g. the many install-step shell commands a test
        // doesn't care about individually) succeed with empty output by default.
        let response = if command.contains("if test -L ") && command.contains("/current") {
            success("-\n-\n")
        } else if command.contains("inspect kamal-proxy --format '{{.State.Status}}'") {
            success("running\n")
        } else {
            self.responses
                .get(&command)
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

async fn spawn_test_server(
    authorized_key: PublicKey,
    responses: HashMap<String, CannedResponse>,
) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
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
        responses,
        pending: Arc::new(Mutex::new(HashMap::new())),
        received: Arc::clone(&received),
    };

    tokio::spawn(async move {
        let _ = test_server.run_on_socket(config, &listener).await;
        drop(listener);
    });

    (addr, received)
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
        stderr: String::new(),
    }
}

fn add_network_setup_responses(responses: &mut HashMap<String, CannedResponse>) {
    responses.insert("id -u".to_string(), success("0\n"));
    let dir = format!(
        "/etc/jiji/network/{}",
        jiji_network::systemd_unit_slug("testproject")
    );
    responses.insert(
        format!("test -s {dir}/public.key && cat {dir}/public.key"),
        success("test-wireguard-public-key\n"),
    );
}

fn write_config(
    dir: &std::path::Path,
    addr: SocketAddr,
    key_path: &std::path::Path,
) -> std::path::PathBuf {
    let config_path = dir.join("deploy.yml");
    std::fs::write(
        &config_path,
        format!(
            r#"
project: testproject
builder:
  engine: docker
servers:
  web1:
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
        ),
    )
    .expect("write test deploy.yml");
    config_path
}

fn run_jiji_server_setup(config_path: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_jiji"))
        .arg("server")
        .arg("setup")
        .arg("-c")
        .arg(config_path)
        .output()
        .expect("run jiji server setup")
}

fn run_jiji_server_setup_with_hosts(
    config_path: &std::path::Path,
    hosts: &str,
) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_jiji"))
        .arg("server")
        .arg("setup")
        .arg("-c")
        .arg(config_path)
        .arg("-H")
        .arg(hosts)
        .output()
        .expect("run jiji server setup")
}

fn run_jiji_proxy_restart(config_path: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_jiji"))
        .arg("proxy")
        .arg("restart")
        .arg("-c")
        .arg(config_path)
        .output()
        .expect("run jiji proxy restart")
}

fn run_jiji_proxy_logs(config_path: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_jiji"))
        .arg("proxy")
        .arg("logs")
        .args(args)
        .arg("-c")
        .arg(config_path)
        .output()
        .expect("run jiji proxy logs")
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_an_already_installed_engine() {
    let client_key =
        PrivateKey::random(&mut rng(), Algorithm::Ed25519).expect("generate client key");
    let mut responses = HashMap::new();
    responses.insert("which docker".to_string(), success(""));
    responses.insert(
        "docker --version".to_string(),
        success("Docker version 99.0.0, build abcdef\n"),
    );
    add_network_setup_responses(&mut responses);

    let (addr, received) = spawn_test_server(client_key.public_key().clone(), responses).await;

    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = dir.path().join("id_ed25519");
    std::fs::write(
        &key_path,
        client_key
            .to_openssh(LineEnding::LF)
            .expect("encode key as openssh")
            .as_bytes(),
    )
    .expect("write key file");

    let config_path = write_config(dir.path(), addr, &key_path);
    let output = run_jiji_server_setup(&config_path);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "expected success, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("connected"), "stdout: {stdout}");
    assert!(
        stdout.contains("docker already installed (Docker version 99.0.0, build abcdef)"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("kamal-proxy configured and running"),
        "stdout: {stdout}"
    );
    assert!(
        received
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.contains("cat >> .jiji/testproject/audit.log")),
        "a successful server setup should append an audit entry"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn hosts_filter_matches_the_configured_server_name_not_just_its_host_address() {
    // `write_config` names the server `web1` with a loopback test-server address as its `host:`
    // -- `-H web1` must match the config-key name (mirroring `NetworkPlan::select_hosts`'s
    // documented name-or-host matching, used by `deploy`/`teardown`), not just `server.host`.
    let client_key =
        PrivateKey::random(&mut rng(), Algorithm::Ed25519).expect("generate client key");
    let mut responses = HashMap::new();
    responses.insert("which docker".to_string(), success(""));
    responses.insert(
        "docker --version".to_string(),
        success("Docker version 99.0.0, build abcdef\n"),
    );
    add_network_setup_responses(&mut responses);

    let (addr, _received) = spawn_test_server(client_key.public_key().clone(), responses).await;

    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = dir.path().join("id_ed25519");
    std::fs::write(
        &key_path,
        client_key
            .to_openssh(LineEnding::LF)
            .expect("encode key as openssh")
            .as_bytes(),
    )
    .expect("write key file");

    let config_path = write_config(dir.path(), addr, &key_path);
    let output = run_jiji_server_setup_with_hosts(&config_path, "missing,web1");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "expected success, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("Targeting 1 server(s): web1"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("Host filter 'missing' matched no servers"),
        "stdout: {stdout}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn installs_a_missing_engine() {
    let client_key =
        PrivateKey::random(&mut rng(), Algorithm::Ed25519).expect("generate client key");
    let mut responses = HashMap::new();
    responses.insert("which docker".to_string(), failure());
    responses.insert(
        "cat /etc/os-release".to_string(),
        success("ID=ubuntu\nVERSION_ID=\"24.04\"\nVERSION_CODENAME=noble\n"),
    );
    responses.insert(
        "docker --version".to_string(),
        success("Docker version 99.0.0, build abcdef\n"),
    );
    add_network_setup_responses(&mut responses);
    // Every install command not explicitly listed defaults to success (empty CannedResponse).

    let (addr, _) = spawn_test_server(client_key.public_key().clone(), responses).await;

    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = dir.path().join("id_ed25519");
    std::fs::write(
        &key_path,
        client_key
            .to_openssh(LineEnding::LF)
            .expect("encode key as openssh")
            .as_bytes(),
    )
    .expect("write key file");

    let config_path = write_config(dir.path(), addr, &key_path);
    let output = run_jiji_server_setup(&config_path);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "expected success, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("docker installed (Docker version 99.0.0, build abcdef)"),
        "stdout: {stdout}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn proxy_restart_forces_pull_remove_and_recreate() {
    let client_key =
        PrivateKey::random(&mut rng(), Algorithm::Ed25519).expect("generate client key");
    let mut responses = HashMap::new();
    responses.insert(
        "docker inspect kamal-proxy --format '{{.State.Status}}'".to_string(),
        success("running\n"),
    );
    let (addr, received) = spawn_test_server(client_key.public_key().clone(), responses).await;

    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = dir.path().join("id_ed25519");
    std::fs::write(
        &key_path,
        client_key
            .to_openssh(LineEnding::LF)
            .expect("encode key as openssh")
            .as_bytes(),
    )
    .expect("write key file");
    let config_path = write_config(dir.path(), addr, &key_path);

    let config: jiji_config::Config =
        serde_yaml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    let plan = jiji_network::NetworkPlanner::new().plan(&config).unwrap();
    let server_plan = &plan.servers["web1"];

    let output = run_jiji_proxy_restart(&config_path);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let commands = received.lock().expect("received mutex poisoned");
    assert!(commands
        .iter()
        .any(|command| command == "docker pull ghcr.io/acidtib/kamal-proxy:jiji"));
    assert!(commands
        .iter()
        .any(|command| command == "docker container rm -f kamal-proxy"));
    assert!(commands.iter().any(|command| command.starts_with(&format!(
        "docker run --name kamal-proxy --network {} --ip {} ",
        server_plan.bridge_name, server_plan.proxy_address
    ))));
    assert!(
        !commands
            .iter()
            .any(|command| command.contains("index .Config.Labels \"jiji.proxy-config\"")),
        "force restart must skip the fingerprint inspection"
    );
    assert!(
        commands.iter().any(|command| command
            == &format!(
                "docker network connect --ip {} {} kamal-proxy",
                server_plan.proxy_address, server_plan.bridge_name
            )),
        "kamal-proxy should be attached to this project's bridge after recreation: {commands:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn proxy_logs_sends_quoted_filters_and_prints_host_output() {
    let client_key =
        PrivateKey::random(&mut rng(), Algorithm::Ed25519).expect("generate client key");
    let command =
        "docker logs --timestamps --since='1 hour ago' kamal-proxy | grep -- 'can'\\''t; echo bad'";
    let mut responses = HashMap::new();
    responses.insert(command.to_string(), success("matched proxy line\n"));
    let (addr, received) = spawn_test_server(client_key.public_key().clone(), responses).await;

    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = dir.path().join("id_ed25519");
    std::fs::write(
        &key_path,
        client_key
            .to_openssh(LineEnding::LF)
            .expect("encode key as openssh")
            .as_bytes(),
    )
    .expect("write key file");
    let config_path = write_config(dir.path(), addr, &key_path);

    let output = run_jiji_proxy_logs(
        &config_path,
        &["--since", "1 hour ago", "--grep", "can't; echo bad"],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("web1:"), "stdout: {stdout}");
    assert!(stdout.contains("matched proxy line"), "stdout: {stdout}");
    assert!(received
        .lock()
        .expect("received mutex poisoned")
        .iter()
        .any(|received| received == command));
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_a_connection_failure_without_touching_the_engine() {
    // No server at all -- connect should fail and the command should exit non-zero with an
    // actionable message, rather than panicking or hanging.
    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = dir.path().join("id_ed25519");
    let client_key =
        PrivateKey::random(&mut rng(), Algorithm::Ed25519).expect("generate client key");
    std::fs::write(
        &key_path,
        client_key
            .to_openssh(LineEnding::LF)
            .expect("encode key as openssh")
            .as_bytes(),
    )
    .expect("write key file");

    // Bind and immediately drop, to get a port nothing is listening on.
    let addr = {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind throwaway listener");
        listener.local_addr().expect("read listener addr")
    };

    let config_path = write_config(dir.path(), addr, &key_path);
    let output = run_jiji_server_setup(&config_path);

    assert!(!output.status.success(), "expected a non-zero exit code");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Server setup failed"), "stderr: {stderr}");
}
