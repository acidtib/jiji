//! Integration tests for `jiji server upgrade`, run as a real subprocess against a real,
//! in-process SSH server (mirroring `server_setup_test.rs`'s pattern), so the full config-load ->
//! connect -> version-read -> compare -> apply path is exercised without touching a real host.

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
    received: Arc<Mutex<Vec<String>>>,
    /// One new SSH/TCP connection per `new_client` call -- lets a test assert a host was never
    /// dialed a second time beyond its single locked/reused session (see the SSH Connection
    /// Management docs on avoiding extra target connections).
    connections: Arc<Mutex<u32>>,
}

impl server::Server for TestServer {
    type Handler = Self;

    fn new_client(&mut self, _peer_addr: Option<SocketAddr>) -> Self {
        *self.connections.lock().expect("connections mutex poisoned") += 1;
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
        _channel: ChannelId,
        _data: &[u8],
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
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
        let response = if command.contains("inspect jiji-proxy --format '{{.State.Status}}'")
            && !command.contains("Config.Labels")
        {
            success("running\n")
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
    connections: Arc<Mutex<u32>>,
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
    let connections = Arc::new(Mutex::new(0));
    let mut test_server = TestServer {
        authorized_key,
        responses: Arc::new(responses),
        pending: Arc::new(Mutex::new(HashMap::new())),
        received: received.clone(),
        connections: connections.clone(),
    };

    tokio::spawn(async move {
        let _ = test_server.run_on_socket(config, &listener).await;
        drop(listener);
    });

    Harness {
        addr,
        received,
        connections,
    }
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

/// Single server "web1", project "testproject" -- every helper in this file is derived from this
/// exact project/server naming.
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

/// Two servers, "web1" and "web2", both reachable at their own address.
fn write_two_server_config(
    dir: &std::path::Path,
    first: SocketAddr,
    second: SocketAddr,
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
    host: {first_ip}
    port: {first_port}
    keys:
      - {key_path}
  web2:
    host: {second_ip}
    port: {second_port}
    keys:
      - {key_path}
services: {{}}
ssh:
  user: tester
  keys_only: true
"#,
            first_ip = first.ip(),
            first_port = first.port(),
            second_ip = second.ip(),
            second_port = second.port(),
            key_path = key_path.display(),
        ),
    )
    .expect("write two-server deploy.yml");
    config_path
}

fn network_dir() -> String {
    format!(
        "/etc/jiji/network/{}",
        jiji_network::systemd_unit_slug("testproject")
    )
}

fn public_key_command() -> String {
    format!("cat {}/public.key", network_dir())
}

fn membership_export_command() -> String {
    let paths = AgentPaths::default_for_project("testproject");
    format!(
        "{} membership-export --state-dir {}",
        paths.binary_path.display(),
        paths.state_dir.display()
    )
}

fn agent_health_command() -> String {
    let paths = AgentPaths::default_for_project("testproject");
    format!(
        "{} request --socket {} # jiji-request:health",
        paths.binary_path.display(),
        paths.socket_path.display()
    )
}

fn agent_health_response(version: &str) -> CannedResponse {
    success(&format!(
        r#"{{"Ok":{{"type":"health","schema_version":1,"observation_count":0,"version":"{version}"}}}}"#
    ))
}

fn proxy_version_command() -> String {
    "docker exec jiji-proxy jiji-proxy version".to_string()
}

fn proxy_fingerprint_command() -> String {
    "docker inspect jiji-proxy --format '{{.State.Status}} {{index .Config.Labels \"jiji.proxy-config\"}} {{.Config.Image}}'".to_string()
}

/// The exact output `proxy::is_current_and_running` expects when the running container already
/// matches this engine's fingerprint and the current `PROXY_VERSION` image tag -- registering this
/// is what makes `ensure_proxy(force: false)` treat an already-current proxy as a no-op.
fn proxy_fingerprint_current_response() -> CannedResponse {
    success(&format!("running v1-docker {}\n", jiji_network::image()))
}

fn agent_binary_install_command() -> String {
    let paths = AgentPaths::default_for_project("testproject");
    format!(
        "install -D -m 0755 /dev/stdin {}",
        paths.binary_path.display()
    )
}

/// Every response common to a fully successful single-host run, regardless of scenario: a
/// resolvable WireGuard public key (so Pass 1 enrollment doesn't fail) and an empty prior
/// membership export.
fn base_responses() -> HashMap<String, CannedResponse> {
    let mut responses = HashMap::new();
    responses.insert(public_key_command(), success("test-wireguard-public-key\n"));
    responses.insert(membership_export_command(), success("[]\n"));
    responses.insert(
        "docker inspect jiji-proxy --format '{{.State.Status}}'".to_string(),
        success("running\n"),
    );
    responses
}

fn run_jiji_server_upgrade(
    config_path: &std::path::Path,
    extra_args: &[&str],
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_jiji"));
    command
        .arg("server")
        .arg("upgrade")
        .arg("-c")
        .arg(config_path)
        .arg("-y");
    command.args(extra_args);
    let dir = config_path.parent().expect("config has a parent dir");
    let fake_binary = dir.join("fake-jiji-agent");
    if !fake_binary.exists() {
        // `resolve_agent_binary_source` execs `{path} version` locally before trusting a
        // discovered binary (see `agent_distribution.rs`), so this placeholder must actually run
        // and report a current version, not just exist as a file.
        std::fs::write(
            &fake_binary,
            format!(
                "#!/bin/sh\nif [ \"$1\" = version ]; then printf '%s\\n' '{}'; fi\nexit 0\n",
                env!("JIJI_AGENT_BUILD_VERSION")
            ),
        )
        .expect("write fake agent binary");
        let mut permissions = std::fs::metadata(&fake_binary)
            .expect("read fake agent binary metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&fake_binary, permissions).expect("chmod fake agent binary");
    }
    command
        .env("JIJI_AGENT_BINARY", &fake_binary)
        .output()
        .expect("run jiji server upgrade")
}

#[tokio::test(flavor = "multi_thread")]
async fn current_versions_are_refreshed_without_recreating_the_proxy() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = base_responses();
    responses.insert(
        agent_health_command(),
        agent_health_response(env!("JIJI_AGENT_BUILD_VERSION")),
    );
    responses.insert(
        proxy_version_command(),
        success(&format!("{}\n", jiji_network::PROXY_VERSION)),
    );
    responses.insert(
        proxy_fingerprint_command(),
        proxy_fingerprint_current_response(),
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);

    let output = run_jiji_server_upgrade(&config_path, &[]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let commands = harness.received.lock().unwrap().clone();
    // Current still refreshes: the binary upload command is present (this mock has no prior
    // remote file, so the hash check always reports a difference; what matters here is that the
    // config/unit/membership refresh tail always runs regardless).
    assert!(
        commands
            .iter()
            .any(|c| c.contains("systemctl enable --now")),
        "expected the agent unit to be (re-)enabled: {commands:?}"
    );
    // The proxy container itself was never removed: `ensure_proxy(force: false)` with a
    // current, already-running container (matching fingerprint registered above) never
    // recreates it.
    assert!(
        !commands
            .iter()
            .any(|c| c.contains("container rm -f jiji-proxy")),
        "a current proxy must not be recreated: {commands:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn outdated_versions_are_upgraded() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = base_responses();
    responses.insert(agent_health_command(), agent_health_response("0.0.1"));
    // Occurrence #1 is this command's own read phase (before recreate: still the old version);
    // the plain (unsuffixed) entry is the fallback every later occurrence uses, including
    // `ensure_proxy`'s own post-recreate `check_version` call -- which, after a real recreate,
    // would be talking to the newly started (current-version) container.
    responses.insert(format!("{}#1", proxy_version_command()), success("0.0.1\n"));
    responses.insert(
        proxy_version_command(),
        success(&format!("{}\n", jiji_network::PROXY_VERSION)),
    );
    responses.insert(
        proxy_fingerprint_command(),
        success("running v1-docker docker.io/example/outdated:v0.0.1\n"),
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);

    let output = run_jiji_server_upgrade(&config_path, &[]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let commands = harness.received.lock().unwrap().clone();
    assert!(
        commands.contains(&agent_binary_install_command()),
        "expected the outdated agent binary to be uploaded: {commands:?}"
    );
    assert!(
        commands
            .iter()
            .any(|c| c.contains("container rm -f jiji-proxy")
                || (c.contains("flock") && c.contains("docker container rm -f jiji-proxy"))),
        "expected the outdated proxy to be recreated: {commands:?}"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("outdated"), "stdout: {stdout}");
}

#[tokio::test(flavor = "multi_thread")]
async fn ahead_versions_are_never_downgraded() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = base_responses();
    responses.insert(agent_health_command(), agent_health_response("99.0.0"));
    responses.insert(proxy_version_command(), success("99.0.0\n"));

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);

    let output = run_jiji_server_upgrade(&config_path, &[]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let commands = harness.received.lock().unwrap().clone();
    assert!(
        !commands.contains(&agent_binary_install_command()),
        "an ahead agent binary must never be touched: {commands:?}"
    );
    assert!(
        !commands
            .iter()
            .any(|c| c.contains("container rm -f jiji-proxy")),
        "an ahead proxy must never be recreated: {commands:?}"
    );
    // The full "never touched" contract, not just "binary not uploaded": an ahead host must
    // never even have its agent unit stopped, its config rewritten, or its membership
    // re-imported -- `ensure_agent` runs all three unconditionally whenever it's called at all,
    // regardless of `binary_path`, so the only way to honor this is to never call it here.
    assert!(
        !commands.iter().any(|c| c.contains("systemctl")),
        "an ahead host's agent unit must never be touched at all: {commands:?}"
    );
    assert!(
        !commands.iter().any(|c| c.contains("membership-import")),
        "an ahead host's membership must never be re-imported: {commands:?}"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("ahead"), "stdout: {stdout}");
    assert!(stdout.contains("not touched"), "stdout: {stdout}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_host_is_reported_unavailable_and_the_command_fails() {
    let (dir, key_path, _client_key) = setup_test_dir();
    // Nothing is listening on this address: connect always fails.
    let unreachable = SocketAddr::from(([127, 0, 0, 1], 1));
    let config_path = write_config(dir.path(), unreachable, &key_path);

    let output = run_jiji_server_upgrade(&config_path, &[]);
    assert!(!output.status.success(), "expected non-zero exit");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("unavailable")
            || stderr.contains("unavailable")
            || stderr.contains("Could not connect"),
        "stdout: {stdout}\nstderr: {stderr}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn one_unreachable_host_does_not_hide_a_reachable_hosts_success() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = base_responses();
    responses.insert(
        agent_health_command(),
        agent_health_response(env!("JIJI_AGENT_BUILD_VERSION")),
    );
    responses.insert(
        proxy_version_command(),
        success(&format!("{}\n", jiji_network::PROXY_VERSION)),
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    // web2 has nothing listening: unreachable.
    let unreachable = SocketAddr::from(([127, 0, 0, 1], 1));
    let config_path = write_two_server_config(dir.path(), harness.addr, unreachable, &key_path);

    let output = run_jiji_server_upgrade(&config_path, &[]);
    assert!(!output.status.success(), "expected non-zero exit overall");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("web1") && (stdout.contains("current") || stdout.contains("refresh")),
        "expected web1's successful read reported: {stdout}"
    );

    let commands = harness.received.lock().unwrap().clone();
    assert!(
        commands
            .iter()
            .any(|c| c.contains("systemctl enable --now")),
        "web1 should still have been upgraded despite web2 being unreachable: {commands:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn services_flag_is_rejected_before_any_ssh_command() {
    let (dir, key_path, client_key) = setup_test_dir();
    let harness = spawn_test_server(client_key.public_key().clone(), HashMap::new()).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);

    let mut command = Command::new(env!("CARGO_BIN_EXE_jiji"));
    command
        .arg("-S")
        .arg("web")
        .arg("server")
        .arg("upgrade")
        .arg("-c")
        .arg(&config_path)
        .arg("-y");
    let output = command.output().expect("run jiji server upgrade -S web");

    assert!(!output.status.success(), "expected non-zero exit");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("does not accept"), "stderr: {stderr}");

    let received = harness.received.lock().unwrap().clone();
    assert!(
        received.is_empty(),
        "expected zero SSH commands: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_agent_binary_upload_is_reported_and_the_command_fails() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = base_responses();
    responses.insert(agent_health_command(), agent_health_response("0.0.1"));
    responses.insert(
        proxy_version_command(),
        success(&format!("{}\n", jiji_network::PROXY_VERSION)),
    );
    responses.insert(
        agent_binary_install_command(),
        CannedResponse {
            success: false,
            stdout: String::new(),
            stderr: "permission denied".to_string(),
        },
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);

    let output = run_jiji_server_upgrade(&config_path, &[]);
    assert!(!output.status.success(), "expected non-zero exit");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("permission denied") || stderr.contains("permission denied"),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    // The proxy component succeeded independently -- a failed agent apply must not hide it.
    assert!(
        stdout.contains("jiji-proxy") && !stdout.contains("jiji-proxy: permission denied"),
        "stdout: {stdout}"
    );
}

/// Regression test: an ahead-of-required host used to be excluded from `upgrade_agents`'s own
/// `target_names`, so both `gather_membership_view`'s fallback and `push_membership_everywhere`
/// treated it as outside this run's scope and opened a second (and third) fresh connection to a
/// host already connected and locked this run -- exactly what the SSH Connection Management docs
/// warn can trip `ufw limit ssh`. It must be reached only through its one already-open session,
/// and (per the "ahead of required -> never touched" contract, `ahead_versions_are_never_downgraded`
/// above) never receive a membership push at all.
#[tokio::test(flavor = "multi_thread")]
async fn an_ahead_host_is_read_once_and_never_pushed_to() {
    let (dir, key_path, client_key) = setup_test_dir();

    let mut web1_responses = base_responses();
    web1_responses.insert(agent_health_command(), agent_health_response("0.0.1"));
    web1_responses.insert(format!("{}#1", proxy_version_command()), success("0.0.1\n"));
    web1_responses.insert(
        proxy_version_command(),
        success(&format!("{}\n", jiji_network::PROXY_VERSION)),
    );
    web1_responses.insert(
        proxy_fingerprint_command(),
        success("running v1-docker docker.io/example/outdated:v0.0.1\n"),
    );
    let web1 = spawn_test_server(client_key.public_key().clone(), web1_responses).await;

    let mut web2_responses = base_responses();
    web2_responses.insert(agent_health_command(), agent_health_response("99.0.0"));
    web2_responses.insert(proxy_version_command(), success("99.0.0\n"));
    let web2 = spawn_test_server(client_key.public_key().clone(), web2_responses).await;

    let config_path = write_two_server_config(dir.path(), web1.addr, web2.addr, &key_path);

    let output = run_jiji_server_upgrade(&config_path, &[]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(
        *web2.connections.lock().unwrap(),
        2,
        "an ahead host must be reached through only its one already-locked session (plus the \
         unrelated, always-happens `jiji network diagnostics` reconnect at the very end) -- never \
         an extra connection to read or push membership"
    );
    let web2_commands = web2.received.lock().unwrap().clone();
    assert!(
        !web2_commands
            .iter()
            .any(|c| c.contains("membership-import")),
        "an ahead host's membership must never be re-imported: {web2_commands:?}"
    );
}

/// Regression test: a host that fails partway through `enroll_agent_targets` (its own separate
/// enrollment connection closes on that failure) used to stay inside `target_names`'s exclusion
/// set regardless, so `push_membership_everywhere`'s mesh-wide fallback below skipped it too --
/// leaving it with silently stale membership and no diagnostic, since the "membership not yet
/// current" notice only ever covered `push_outcome.unreachable`. It must still receive a
/// membership push, through its still-open `HostRuntime`-locked session from `run()` (distinct
/// from the enrollment attempt's own connection), and the command must still fail overall for
/// the enroll error itself.
#[tokio::test(flavor = "multi_thread")]
async fn an_enroll_failure_still_gets_membership_pushed_through_its_lock_session() {
    let (dir, key_path, client_key) = setup_test_dir();

    let mut web1_responses = base_responses();
    web1_responses.insert(agent_health_command(), agent_health_response("0.0.1"));
    web1_responses.insert(format!("{}#1", proxy_version_command()), success("0.0.1\n"));
    web1_responses.insert(
        proxy_version_command(),
        success(&format!("{}\n", jiji_network::PROXY_VERSION)),
    );
    web1_responses.insert(
        proxy_fingerprint_command(),
        success("running v1-docker docker.io/example/outdated:v0.0.1\n"),
    );
    let web1 = spawn_test_server(client_key.public_key().clone(), web1_responses).await;

    let mut web2_responses = base_responses();
    web2_responses.insert(agent_health_command(), agent_health_response("0.0.1"));
    web2_responses.insert(
        proxy_version_command(),
        success(&format!("{}\n", jiji_network::PROXY_VERSION)),
    );
    web2_responses.insert(
        proxy_fingerprint_command(),
        proxy_fingerprint_current_response(),
    );
    // web2's WireGuard public key is unavailable: `enroll_agent_targets` fails partway through
    // enrolling it (closing that attempt's own separate connection) before it ever reaches the
    // install/push loop.
    web2_responses.insert(
        public_key_command(),
        CannedResponse {
            success: false,
            stdout: String::new(),
            stderr: "no such file".to_string(),
        },
    );
    let web2 = spawn_test_server(client_key.public_key().clone(), web2_responses).await;

    let config_path = write_two_server_config(dir.path(), web1.addr, web2.addr, &key_path);

    let output = run_jiji_server_upgrade(&config_path, &[]);
    assert!(!output.status.success(), "expected non-zero exit overall");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("web2") && stdout.contains("WireGuard public key is unavailable"),
        "expected web2's enroll failure reported: {stdout}"
    );

    let web2_commands = web2.received.lock().unwrap().clone();
    assert!(
        web2_commands
            .iter()
            .any(|c| c.contains("membership-import")),
        "web2 should still have received a membership push through its already-open lock \
         session despite the enroll failure: {web2_commands:?}"
    );
    assert_eq!(
        *web2.connections.lock().unwrap(),
        3,
        "web2 should only ever see its one locked session (reused for the membership push), \
         enroll_agent_targets's own separate, failed enrollment attempt, and the unrelated, \
         always-happens `jiji network diagnostics` reconnect at the very end -- never a fourth \
         connection: {web2_commands:?}"
    );

    let web1_commands = web1.received.lock().unwrap().clone();
    assert!(
        web1_commands
            .iter()
            .any(|c| c.contains("systemctl enable --now")),
        "web1 should still have been upgraded despite web2's enroll failure: {web1_commands:?}"
    );
}
