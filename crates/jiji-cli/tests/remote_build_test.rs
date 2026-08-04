//! Integration tests for remote (`builder.remote`) builds, run as a real subprocess against a
//! real, in-process SSH server (mirroring `server_setup_test.rs`/`deploy_test.rs`'s pattern), so
//! the full connect -> preflight -> stage -> build -> push -> cleanup path is exercised without
//! touching a real builder host.

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

/// (command, stdin bytes) pairs recorded per completed channel.
type ReceivedStdin = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

#[derive(Clone, Default)]
struct CannedResponse {
    success: bool,
    stdout: String,
    stderr: String,
    /// `false` simulates a signal-killed remote command: the channel closes without ever
    /// sending `ChannelMsg::ExitStatus`, which `jiji-ssh` (and every consumer of it) must treat
    /// the same as a failure, never as success.
    send_exit_status: bool,
}

fn success(stdout: &str) -> CannedResponse {
    CannedResponse {
        success: true,
        stdout: stdout.to_string(),
        stderr: String::new(),
        send_exit_status: true,
    }
}

fn failure() -> CannedResponse {
    CannedResponse {
        success: false,
        stdout: String::new(),
        stderr: "boom".to_string(),
        send_exit_status: true,
    }
}

fn no_exit_status() -> CannedResponse {
    CannedResponse {
        success: false,
        stdout: String::new(),
        stderr: String::new(),
        send_exit_status: false,
    }
}

fn default_response(command: &str) -> CannedResponse {
    let body = if command.contains("# jiji-request:catalog-list") {
        r#"{"Ok":{"type":"catalog_list","records":[]}}"#
    } else if command.contains("# jiji-request:desired-read") {
        r#"{"Ok":{"type":"desired_state","record":null}}"#
    } else if command.contains("# jiji-request:allocate-address") {
        r#"{"Ok":{"type":"address_lease","deployment_id":"test-deploy","replica_id":"web-test","address":"100.64.0.10","state":"active"}}"#
    } else if command.contains("# jiji-request:catalog-commit") {
        r#"{"Ok":{"type":"catalog_committed","record":{"project_id":"demo","recovery_epoch":1,"protocol_version":1,"schema_version":2,"service":"web","replica_id":"web-test","owner_node_id":"app","owner_epoch":1,"revision":1,"deployment_id":"test-deploy","address":"100.64.0.10","ports":[],"image":"registry.example.com/demo-web:v1","state":"active","health":"healthy"}}}"#
    } else if command.contains("# jiji-request:release-address") {
        r#"{"Ok":{"type":"address_released","released":true}}"#
    } else {
        ""
    };
    success(body)
}

#[derive(Clone)]
struct TestServer {
    authorized_key: PublicKey,
    responses: Arc<HashMap<String, CannedResponse>>,
    pending: Arc<Mutex<HashMap<ChannelId, String>>>,
    received: Arc<Mutex<Vec<String>>>,
    stdin: Arc<Mutex<HashMap<ChannelId, Vec<u8>>>>,
    /// Keyed by command rather than flattened into one buffer, so a test can look up exactly one
    /// command's stdin (e.g. a login) without it being conflated with another command's stdin
    /// (e.g. a tar upload) in the same session.
    received_stdin: ReceivedStdin,
    forwards: Arc<Mutex<Vec<(String, u32)>>>,
    cancelled_forwards: Arc<Mutex<Vec<(String, u32)>>>,
    /// Simulates a builder-side port conflict: the reverse-forward request is rejected outright.
    reject_forward: bool,
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

    async fn tcpip_forward(
        &mut self,
        address: &str,
        port: &mut u32,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        if self.reject_forward {
            return Ok(false);
        }
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

        if let Some(stdin) = self
            .stdin
            .lock()
            .expect("stdin mutex poisoned")
            .remove(&channel)
        {
            self.received_stdin
                .lock()
                .expect("received_stdin mutex poisoned")
                .push((command.clone(), stdin));
        }

        // Per-occurrence overrides (`"{command}#{occurrence}"`) let a test give the same command
        // string different canned responses across repeated calls (e.g. a `buildx inspect` that
        // fails the first time and succeeds on retry), mirroring `deploy_test.rs`'s pattern.
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
            .unwrap_or_else(|| default_response(&command));

        if !response.stdout.is_empty() {
            session.data(channel, response.stdout)?;
        }
        if !response.stderr.is_empty() {
            session.extended_data(channel, 1, response.stderr)?;
        }
        if response.send_exit_status {
            session.exit_status_request(channel, if response.success { 0 } else { 1 })?;
        }
        session.eof(channel)?;
        session.close(channel)?;
        Ok(())
    }
}

struct Harness {
    addr: SocketAddr,
    received: Arc<Mutex<Vec<String>>>,
    received_stdin: ReceivedStdin,
    forwards: Arc<Mutex<Vec<(String, u32)>>>,
    cancelled_forwards: Arc<Mutex<Vec<(String, u32)>>>,
}

async fn spawn_test_server(
    authorized_key: PublicKey,
    responses: HashMap<String, CannedResponse>,
) -> Harness {
    spawn_test_server_with(authorized_key, responses, false).await
}

async fn spawn_test_server_with(
    authorized_key: PublicKey,
    responses: HashMap<String, CannedResponse>,
    reject_forward: bool,
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
    let forwards = Arc::new(Mutex::new(Vec::new()));
    let cancelled_forwards = Arc::new(Mutex::new(Vec::new()));
    let mut test_server = TestServer {
        authorized_key,
        responses: Arc::new(responses),
        pending: Arc::new(Mutex::new(HashMap::new())),
        received: received.clone(),
        stdin: Arc::new(Mutex::new(HashMap::new())),
        received_stdin: received_stdin.clone(),
        forwards: forwards.clone(),
        cancelled_forwards: cancelled_forwards.clone(),
        reject_forward,
    };

    tokio::spawn(async move {
        let _ = test_server.run_on_socket(config, &listener).await;
        drop(listener);
    });

    Harness {
        addr,
        received,
        received_stdin,
        forwards,
        cancelled_forwards,
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

/// `project_root_from_config_path` assumes `<project_root>/.jiji/<file>.yml`, and the remote
/// build context is packaged from real local files, so both must exist for real (unlike other
/// integration tests that never read the local build context).
fn write_project(
    dir: &std::path::Path,
    addr: SocketAddr,
    key_path: &std::path::Path,
) -> std::path::PathBuf {
    write_project_with_registry(
        dir,
        addr,
        key_path,
        "    type: remote\n    server: registry.example.com\n",
    )
}

fn write_project_with_registry(
    dir: &std::path::Path,
    addr: SocketAddr,
    key_path: &std::path::Path,
    registry_section: &str,
) -> std::path::PathBuf {
    let jiji_dir = dir.join(".jiji");
    std::fs::create_dir_all(&jiji_dir).expect("create .jiji dir");
    std::fs::write(dir.join("Dockerfile"), "FROM scratch\n").expect("write Dockerfile");

    let config_path = jiji_dir.join("deploy.yml");
    std::fs::write(
        &config_path,
        format!(
            r#"
project: testproject
builder:
  engine: docker
  local: false
  remote: ssh://tester@{ip}:{port}
  registry:
{registry_section}servers:
  app:
    host: 10.0.0.1
services:
  web:
    build: .
    servers: [app]
ssh:
  user: tester
  keys_only: true
  keys: [{key_path}]
"#,
            ip = addr.ip(),
            port = addr.port(),
            key_path = key_path.display(),
        ),
    )
    .expect("write test deploy.yml");
    config_path
}

/// Two servers of different architectures, so `required_arches` yields `[linux/amd64,
/// linux/arm64]` and the build takes the `MultiArch` path.
fn write_multi_arch_project(
    dir: &std::path::Path,
    addr: SocketAddr,
    key_path: &std::path::Path,
    engine: &str,
) -> std::path::PathBuf {
    let jiji_dir = dir.join(".jiji");
    std::fs::create_dir_all(&jiji_dir).expect("create .jiji dir");
    std::fs::write(dir.join("Dockerfile"), "FROM scratch\n").expect("write Dockerfile");

    let config_path = jiji_dir.join("deploy.yml");
    std::fs::write(
        &config_path,
        format!(
            r#"
project: testproject
builder:
  engine: {engine}
  local: false
  remote: ssh://tester@{ip}:{port}
  registry:
    type: remote
    server: registry.example.com
servers:
  app1:
    host: 10.0.0.1
  app2:
    host: 10.0.0.2
    arch: arm64
services:
  web:
    build: .
    servers: [app1, app2]
ssh:
  user: tester
  keys_only: true
  keys: [{key_path}]
"#,
            ip = addr.ip(),
            port = addr.port(),
            key_path = key_path.display(),
        ),
    )
    .expect("write test deploy.yml");
    config_path
}

/// Mirrors `remote_build.rs::shell_command` / `registry::shell_quote` exactly, so expected
/// command strings can be built programmatically instead of hand-quoted (both are private to
/// `jiji-cli` and unreachable from this external test crate).
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn remote_command(engine: &str, args: &[&str]) -> String {
    let mut command = engine.to_string();
    for arg in args {
        command.push(' ');
        command.push_str(&quote(arg));
    }
    command
}

fn staging_root_command() -> String {
    "set -eu; mkdir -p .jiji/testproject/builds; mktemp -d .jiji/testproject/builds/run.XXXXXX"
        .to_string()
}

fn staging_root_response(suffix: &str) -> CannedResponse {
    success(&format!(".jiji/testproject/builds/run.{suffix}\n"))
}

fn context_upload_command(staging_root: &str) -> String {
    let remote_context = format!("{staging_root}/context/web");
    format!("set -eu; mkdir -m 0700 -p {remote_context}; tar -C {remote_context} -xf -")
}

fn build_command(staging_root: &str, tags: &[&str]) -> String {
    let remote_context = format!("{staging_root}/context/web");
    let mut args = vec![
        "build".to_string(),
        "-f".to_string(),
        format!("{remote_context}/Dockerfile"),
    ];
    for tag in tags {
        args.push("-t".to_string());
        args.push(tag.to_string());
    }
    args.push(remote_context);
    remote_command(
        "docker",
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
    )
}

fn buildx_inspect_command(builder_name: &str) -> String {
    remote_command("docker", &["buildx", "inspect", builder_name])
}

fn buildx_create_command(builder_name: &str) -> String {
    remote_command(
        "docker",
        &[
            "buildx",
            "create",
            "--name",
            builder_name,
            "--driver",
            "docker-container",
            "--bootstrap",
        ],
    )
}

fn buildx_build_command(staging_root: &str, builder_name: &str, tags: &[&str]) -> String {
    let remote_context = format!("{staging_root}/context/web");
    let mut args = vec![
        "buildx".to_string(),
        "build".to_string(),
        "--builder".to_string(),
        builder_name.to_string(),
        "--platform".to_string(),
        "linux/amd64,linux/arm64".to_string(),
        "-f".to_string(),
        format!("{remote_context}/Dockerfile"),
    ];
    for tag in tags {
        args.push("-t".to_string());
        args.push(tag.to_string());
    }
    args.push("--push".to_string());
    args.push(remote_context);
    remote_command(
        "docker",
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
    )
}

fn manifest_rm_command(manifest: &str) -> String {
    remote_command("podman", &["manifest", "rm", manifest])
}

fn manifest_create_command(manifest: &str) -> String {
    remote_command("podman", &["manifest", "create", manifest])
}

fn podman_arch_build_command(staging_root: &str, platform: &str, manifest: &str) -> String {
    let remote_context = format!("{staging_root}/context/web");
    remote_command(
        "podman",
        &[
            "build",
            "--platform",
            platform,
            "-f",
            &format!("{remote_context}/Dockerfile"),
            "--manifest",
            manifest,
            &remote_context,
        ],
    )
}

fn manifest_push_command(manifest: &str, tag: &str) -> String {
    remote_command(
        "podman",
        &[
            "manifest",
            "push",
            "--all",
            manifest,
            &format!("docker://{tag}"),
        ],
    )
}

fn push_command(tag: &str) -> String {
    remote_command("docker", &["push", tag])
}

fn cleanup_command(staging_root: &str) -> String {
    format!("rm -rf {staging_root}")
}

/// Mirrors `registry::render_login_command` exactly (private to `jiji-cli`, unreachable here).
fn login_command(server: &str, username: &str) -> String {
    format!(
        "docker login {} --username {} --password-stdin",
        quote(server),
        quote(username)
    )
}

/// `registry::ensure_local_registry` always runs on the *local* (test-runner) machine,
/// regardless of `builder.local` -- a local-registry test needs a real fake `docker` on PATH and
/// a real TCP responder at `127.0.0.1:<port>/v2/`, matching `deploy_test.rs`'s
/// `run_local_registry_deploy` pattern, even though the tunnel/build/push under test all happen
/// remotely over SSH.
async fn spawn_local_registry_responder() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind registry");
    let port = listener.local_addr().expect("registry address").port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut request = [0_u8; 256];
                let _ = stream.read(&mut request).await;
                let _ = stream
                    .write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 2\r\n\r\n{}")
                    .await;
            });
        }
    });
    port
}

fn write_local_fake_docker(dir: &std::path::Path, registry_port: u16) -> std::path::PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).expect("create bin");
    let docker = bin.join("docker");
    std::fs::write(
        &docker,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then exit 0; fi\nif [ \"$1\" = \"container\" ] && [ \"$2\" = \"inspect\" ]; then printf 'true|registry|{registry_port}|true\\n'; exit 0; fi\nexit 0\n"
        ),
    )
    .expect("write fake docker");
    let mut permissions = std::fs::metadata(&docker).expect("metadata").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&docker, permissions).expect("chmod");
    bin
}

fn run_build_with_local_bin(
    config_path: &std::path::Path,
    bin: &std::path::Path,
    extra: &[&str],
) -> std::process::Output {
    let existing_path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![bin.to_path_buf()];
    paths.extend(std::env::split_paths(&existing_path));
    let mut command = Command::new(env!("CARGO_BIN_EXE_jiji"));
    command
        .arg("build")
        .arg("-c")
        .arg(config_path)
        .env("PATH", std::env::join_paths(paths).expect("join PATH"));
    command.args(extra);
    command.output().expect("run jiji build")
}

fn run_build(config_path: &std::path::Path, extra: &[&str]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_jiji"));
    command.arg("build").arg("-c").arg(config_path);
    command.args(extra);
    command.output().expect("run jiji build")
}

const TAG_V1: &str = "registry.example.com/testproject-web:v1";
const TAG_LATEST: &str = "registry.example.com/testproject-web:latest";

fn local_tags(registry_port: u16) -> (String, String) {
    (
        format!("localhost:{registry_port}/testproject-web:v1"),
        format!("localhost:{registry_port}/testproject-web:latest"),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn preflight_failure_is_reported_before_any_upload() {
    // A too-old already-installed engine still rejects outright (unlike a missing engine, which
    // `preflight` now installs -- see `missing_engine_on_the_builder_is_installed_before_building`
    // below).
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert("which docker".to_string(), success(""));
    responses.insert(
        "docker --version".to_string(),
        success("Docker version 1.0.0, build abcdef\n"),
    );
    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_project(dir.path(), harness.addr, &key_path);

    let output = run_build(&config_path, &["--version", "v1"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("requires at least"), "stderr: {stderr}");

    let received = harness.received.lock().unwrap().clone();
    assert!(
        !received.iter().any(|c| c.contains("mktemp")),
        "no staging should happen after a failed preflight: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_engine_on_the_builder_is_installed_before_building() {
    // jiji provisions a missing engine on a `builder.remote` host the same way `jiji server
    // setup` does for a deployment host -- unlike the "never installs anything on a builder"
    // behavior this replaced, only multi-arch tooling (Buildx/`podman manifest`) stays
    // detect-and-report only.
    let (dir, key_path, client_key) = setup_test_dir();
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
    responses.insert(staging_root_command(), staging_root_response("abc123"));
    // Every install command not explicitly listed defaults to success (empty CannedResponse).
    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_project(dir.path(), harness.addr, &key_path);

    let output = run_build(&config_path, &["--version", "v1"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Docker version 99.0.0, build abcdef installed on"),
        "stdout: {stdout}"
    );

    let staging_root = ".jiji/testproject/builds/run.abc123";
    let received = harness.received.lock().unwrap().clone();
    assert!(
        received
            .iter()
            .any(|c| c == &context_upload_command(staging_root)),
        "the build should still proceed after installing the engine: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn staging_upload_build_and_push_happen_in_order() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert(staging_root_command(), staging_root_response("abc123"));
    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_project(dir.path(), harness.addr, &key_path);

    let output = run_build(&config_path, &["--version", "v1"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let staging_root = ".jiji/testproject/builds/run.abc123";
    let received = harness.received.lock().unwrap().clone();
    let position = |command: &str| {
        received
            .iter()
            .position(|c| c == command)
            .unwrap_or_else(|| panic!("expected '{command}' in {received:?}"))
    };

    let mktemp = position(&staging_root_command());
    let upload = position(&context_upload_command(staging_root));
    let build = position(&build_command(staging_root, &[TAG_V1, TAG_LATEST]));
    let push1 = position(&push_command(TAG_V1));
    let push2 = position(&push_command(TAG_LATEST));
    let cleanup = position(&cleanup_command(staging_root));

    assert!(mktemp < upload, "{received:?}");
    assert!(upload < build, "{received:?}");
    assert!(build < push1, "{received:?}");
    assert!(push1 < push2, "{received:?}");
    assert!(push2 < cleanup, "{received:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn streamed_stdout_and_stderr_are_forwarded() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert(staging_root_command(), staging_root_response("abc123"));
    let staging_root = ".jiji/testproject/builds/run.abc123";
    responses.insert(
        build_command(staging_root, &[TAG_V1, TAG_LATEST]),
        CannedResponse {
            success: true,
            stdout: "Step 1/1 : FROM scratch\n".to_string(),
            stderr: "warning: something\n".to_string(),
            send_exit_status: true,
        },
    );
    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_project(dir.path(), harness.addr, &key_path);

    let output = run_build(&config_path, &["--version", "v1"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Step 1/1 : FROM scratch"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("warning: something"));
}

#[tokio::test(flavor = "multi_thread")]
async fn nonzero_exit_status_is_a_build_failure() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert(staging_root_command(), staging_root_response("abc123"));
    let staging_root = ".jiji/testproject/builds/run.abc123";
    responses.insert(
        build_command(staging_root, &[TAG_V1, TAG_LATEST]),
        failure(),
    );
    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_project(dir.path(), harness.addr, &key_path);

    let output = run_build(&config_path, &["--version", "v1"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("failed"), "stderr: {stderr}");

    let received = harness.received.lock().unwrap().clone();
    assert!(
        !received.iter().any(|c| c == &push_command(TAG_V1)),
        "a failed build must never push: {received:?}"
    );
    assert!(
        received.iter().any(|c| c == &cleanup_command(staging_root)),
        "cleanup must still run after a failed build: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_exit_status_is_treated_as_a_failure() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert(staging_root_command(), staging_root_response("abc123"));
    let staging_root = ".jiji/testproject/builds/run.abc123";
    responses.insert(
        build_command(staging_root, &[TAG_V1, TAG_LATEST]),
        no_exit_status(),
    );
    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_project(dir.path(), harness.addr, &key_path);

    let output = run_build(&config_path, &["--version", "v1"]);
    assert!(!output.status.success());
    // `commands/build.rs` wraps every per-service error in `Ui::error`, which (matching this
    // codebase's error-printing convention everywhere else) only ever prints the outermost
    // `anyhow::Context` message via `Display`, not the full cause chain -- so the specific "did
    // not report an exit status" detail from `remote_build.rs::stream` isn't visible here, only
    // that the service's build failed at all.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Build failed for service 'web'"),
        "stderr: {stderr}"
    );

    let received = harness.received.lock().unwrap().clone();
    assert!(
        received.iter().any(|c| c == &cleanup_command(staging_root)),
        "cleanup must still run: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cleanup_runs_after_a_failed_context_upload() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert(staging_root_command(), staging_root_response("abc123"));
    let staging_root = ".jiji/testproject/builds/run.abc123";
    responses.insert(context_upload_command(staging_root), failure());
    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_project(dir.path(), harness.addr, &key_path);

    let output = run_build(&config_path, &["--version", "v1"]);
    assert!(!output.status.success());

    let received = harness.received.lock().unwrap().clone();
    assert!(
        !received
            .iter()
            .any(|c| c == &build_command(staging_root, &[TAG_V1, TAG_LATEST])),
        "a failed upload must never reach the build command: {received:?}"
    );
    assert!(
        received.iter().any(|c| c == &cleanup_command(staging_root)),
        "cleanup must still run after a failed upload: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn no_push_leaves_the_image_only_on_the_builder() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert(staging_root_command(), staging_root_response("abc123"));
    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_project(dir.path(), harness.addr, &key_path);

    let output = run_build(&config_path, &["--version", "v1", "--no-push"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let staging_root = ".jiji/testproject/builds/run.abc123";
    let received = harness.received.lock().unwrap().clone();
    assert!(!received.iter().any(|c| c.starts_with("docker 'push'")));
    assert!(received.iter().any(|c| c == &cleanup_command(staging_root)));

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("built remotely, not pushed"),
        "stdout: {stdout}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn registry_password_travels_only_via_stdin() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert(staging_root_command(), staging_root_response("abc123"));
    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_project_with_registry(
        dir.path(),
        harness.addr,
        &key_path,
        "    type: remote\n    server: registry.example.com\n    username: bob\n    password: hunter2\n",
    );

    let output = run_build(&config_path, &["--version", "v1"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let received = harness.received.lock().unwrap().clone();
    assert!(
        !received.iter().any(|c| c.contains("hunter2")),
        "the password must never appear in a command string: {received:?}"
    );
    let login = login_command("registry.example.com", "bob");
    assert!(
        received.iter().any(|c| c == &login),
        "expected a login command on the builder: {received:?}"
    );

    let received_stdin = harness.received_stdin.lock().unwrap().clone();
    let login_stdin = received_stdin
        .iter()
        .find(|(command, _)| command == &login)
        .map(|(_, bytes)| bytes.clone())
        .expect("expected stdin captured for the login command");
    assert_eq!(login_stdin, b"hunter2");
}

#[tokio::test(flavor = "multi_thread")]
async fn local_registry_tunnel_is_opened_and_cancelled_on_finish() {
    let (dir, key_path, client_key) = setup_test_dir();
    let registry_port = spawn_local_registry_responder().await;
    let bin = write_local_fake_docker(dir.path(), registry_port);

    let mut responses = HashMap::new();
    responses.insert(staging_root_command(), staging_root_response("abc123"));
    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_project_with_registry(
        dir.path(),
        harness.addr,
        &key_path,
        &format!("    type: local\n    port: {registry_port}\n"),
    );

    let output = run_build_with_local_bin(&config_path, &bin, &["--version", "v1"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let forwards = harness.forwards.lock().unwrap().clone();
    assert!(
        forwards
            .iter()
            .any(|(_, port)| *port == u32::from(registry_port)),
        "expected a reverse forward for the registry port: {forwards:?}"
    );
    let cancelled = harness.cancelled_forwards.lock().unwrap().clone();
    assert!(
        cancelled
            .iter()
            .any(|(_, port)| *port == u32::from(registry_port)),
        "expected the forward to be cancelled during cleanup: {cancelled:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn tunnel_is_cancelled_even_after_a_failed_build() {
    let (dir, key_path, client_key) = setup_test_dir();
    let registry_port = spawn_local_registry_responder().await;
    let bin = write_local_fake_docker(dir.path(), registry_port);

    let mut responses = HashMap::new();
    responses.insert(staging_root_command(), staging_root_response("abc123"));
    let staging_root = ".jiji/testproject/builds/run.abc123";
    let (tag_v1, tag_latest) = local_tags(registry_port);
    responses.insert(
        build_command(staging_root, &[&tag_v1, &tag_latest]),
        failure(),
    );
    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_project_with_registry(
        dir.path(),
        harness.addr,
        &key_path,
        &format!("    type: local\n    port: {registry_port}\n"),
    );

    let output = run_build_with_local_bin(&config_path, &bin, &["--version", "v1"]);
    assert!(!output.status.success());

    let cancelled = harness.cancelled_forwards.lock().unwrap().clone();
    assert!(
        cancelled
            .iter()
            .any(|(_, port)| *port == u32::from(registry_port)),
        "the tunnel must be cancelled even when the build itself failed: {cancelled:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn port_conflict_on_the_builder_is_reported_actionably() {
    let (dir, key_path, client_key) = setup_test_dir();
    let registry_port = spawn_local_registry_responder().await;
    let bin = write_local_fake_docker(dir.path(), registry_port);

    let mut responses = HashMap::new();
    responses.insert(staging_root_command(), staging_root_response("abc123"));
    let harness = spawn_test_server_with(client_key.public_key().clone(), responses, true).await;
    let config_path = write_project_with_registry(
        dir.path(),
        harness.addr,
        &key_path,
        &format!("    type: local\n    port: {registry_port}\n"),
    );

    let output = run_build_with_local_bin(&config_path, &bin, &["--version", "v1"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("builder.registry.port"), "stderr: {stderr}");

    let staging_root = ".jiji/testproject/builds/run.abc123";
    let received = harness.received.lock().unwrap().clone();
    assert!(
        !received
            .iter()
            .any(|c| c == &context_upload_command(staging_root)),
        "no context should be uploaded after a tunnel failure: {received:?}"
    );
    assert!(
        received.iter().any(|c| c == &cleanup_command(staging_root)),
        "the staging root should still be cleaned up: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn docker_buildx_remote_sequence_creates_the_builder_and_builds_every_platform() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert(staging_root_command(), staging_root_response("abc123"));
    let staging_root = ".jiji/testproject/builds/run.abc123";
    let builder_name = "jiji-builder-testproject";
    responses.insert(buildx_inspect_command(builder_name), failure());
    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_multi_arch_project(dir.path(), harness.addr, &key_path, "docker");

    let output = run_build(&config_path, &["--version", "v1"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let received = harness.received.lock().unwrap().clone();
    let position = |command: &str| {
        received
            .iter()
            .position(|c| c == command)
            .unwrap_or_else(|| panic!("expected '{command}' in {received:?}"))
    };
    let inspect = position(&buildx_inspect_command(builder_name));
    let create = position(&buildx_create_command(builder_name));
    let build = position(&buildx_build_command(
        staging_root,
        builder_name,
        &[TAG_V1, TAG_LATEST],
    ));
    let cleanup = position(&cleanup_command(staging_root));
    assert!(inspect < create, "{received:?}");
    assert!(create < build, "{received:?}");
    assert!(build < cleanup, "{received:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn docker_buildx_remote_tolerates_a_concurrent_builder_create_race() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert(staging_root_command(), staging_root_response("abc123"));
    let builder_name = "jiji-builder-testproject";
    responses.insert(
        format!("{}#1", buildx_inspect_command(builder_name)),
        failure(),
    );
    responses.insert(buildx_create_command(builder_name), failure());
    responses.insert(
        format!("{}#2", buildx_inspect_command(builder_name)),
        success(""),
    );
    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_multi_arch_project(dir.path(), harness.addr, &key_path, "docker");

    let output = run_build(&config_path, &["--version", "v1"]);
    assert!(
        output.status.success(),
        "a lost create race should not fail the build once the builder exists: stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let received = harness.received.lock().unwrap().clone();
    let inspect_count = received
        .iter()
        .filter(|c| *c == &buildx_inspect_command(builder_name))
        .count();
    assert_eq!(
        inspect_count, 2,
        "expected an initial inspect and a retry inspect after the lost race: {received:?}"
    );
    assert!(
        received
            .iter()
            .any(|c| c.starts_with("docker 'buildx' 'build'")),
        "the build must still proceed after the race resolves: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn podman_manifest_remote_sequence_builds_each_platform_and_pushes_the_manifest() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert(staging_root_command(), staging_root_response("abc123"));
    let staging_root = ".jiji/testproject/builds/run.abc123";
    let manifest = "jiji-testproject-web-build";
    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_multi_arch_project(dir.path(), harness.addr, &key_path, "podman");

    let output = run_build(&config_path, &["--version", "v1"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let received = harness.received.lock().unwrap().clone();
    let position = |command: &str| {
        received
            .iter()
            .position(|c| c == command)
            .unwrap_or_else(|| panic!("expected '{command}' in {received:?}"))
    };
    let rm = position(&manifest_rm_command(manifest));
    let create = position(&manifest_create_command(manifest));
    let build_amd64 = position(&podman_arch_build_command(
        staging_root,
        "linux/amd64",
        manifest,
    ));
    let build_arm64 = position(&podman_arch_build_command(
        staging_root,
        "linux/arm64",
        manifest,
    ));
    let push_v1 = position(&manifest_push_command(manifest, TAG_V1));
    let push_latest = position(&manifest_push_command(manifest, TAG_LATEST));
    let cleanup = position(&cleanup_command(staging_root));

    assert!(rm < create, "{received:?}");
    assert!(create < build_amd64, "{received:?}");
    assert!(build_amd64 < build_arm64, "{received:?}");
    assert!(build_arm64 < push_v1, "{received:?}");
    assert!(push_v1 < push_latest, "{received:?}");
    assert!(push_latest < cleanup, "{received:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn jiji_deploy_build_routes_through_the_remote_builder() {
    let (dir, key_path, client_key) = setup_test_dir();
    std::fs::write(dir.path().join("Dockerfile"), "FROM scratch\n").expect("write Dockerfile");
    let jiji_dir = dir.path().join(".jiji");
    std::fs::create_dir_all(&jiji_dir).expect("create .jiji dir");

    let config_template = r#"
project: demo
builder:
  engine: docker
  local: false
  remote: ssh://tester@BUILDER_ADDR
  registry:
    type: remote
    server: registry.example.com
    username: bob
    password: hunter2
servers:
  app:
    host: 127.0.0.1
    port: APP_PORT
    keys: [KEY_PATH]
services:
  web:
    build: .
    servers: [app]
ssh:
  user: tester
  keys_only: true
  keys: [KEY_PATH]
"#
    .replace("KEY_PATH", &key_path.display().to_string());

    let generation_config: Config = serde_yaml::from_str(
        &config_template
            .replace("BUILDER_ADDR", "127.0.0.1:0")
            .replace("APP_PORT", "0"),
    )
    .expect("parse generation config");
    let network_plan = NetworkPlanner::new()
        .plan(&generation_config)
        .expect("network plan");
    let generation = network_plan.mesh_generation;
    let service_runtime_generation = generation.clone();
    let slug = jiji_network::systemd_unit_slug("demo");
    let network_dir = format!("/etc/jiji/network/{slug}");

    let candidate_name = "demo-web-a";
    let staging_root = ".jiji/demo/builds/run.dep456";
    let mut responses = HashMap::new();
    responses.insert(
        format!("cat {network_dir}/mesh-generation 2>/dev/null || true"),
        success(&format!("{generation}\n")),
    );
    responses.insert(
        format!("cat {network_dir}/service-runtime-generation 2>/dev/null || true"),
        success(&format!("{service_runtime_generation}\n")),
    );
    responses.insert(
        format!("cat {network_dir}/service-nat-current/active-slots"),
        success(""),
    );
    responses.insert(
        format!("docker inspect {candidate_name} --format '{{{{.State.Status}}}}'"),
        failure(),
    );
    responses.insert(
        format!(
            "docker inspect {candidate_name} --format '{{{{.State.Status}}}}' | grep -qx running"
        ),
        success(""),
    );
    responses.insert(
        format!("mktemp -d {network_dir}/service-nat-generations/cutover.XXXXXX"),
        success(&format!(
            "{network_dir}/service-nat-generations/cutover.dep789\n"
        )),
    );
    responses.insert(
        "set -eu; mkdir -p .jiji/demo/builds; mktemp -d .jiji/demo/builds/run.XXXXXX".to_string(),
        success(&format!("{staging_root}\n")),
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config = config_template
        .replace("BUILDER_ADDR", &harness.addr.to_string())
        .replace("APP_PORT", &harness.addr.port().to_string());
    let config_path = jiji_dir.join("deploy.yml");
    std::fs::write(&config_path, config).expect("write test deploy.yml");

    // `deploy.rs`'s remote-registry `--build` path still logs in on the *local* machine (moving
    // this to the selected executor is a known follow-up, not yet done) -- a fake local `docker`
    // avoids that step touching the real network.
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).expect("create bin");
    let docker = bin.join("docker");
    std::fs::write(
        &docker,
        r#"#!/bin/sh
if [ "$1" = "login" ]; then
  cat > /dev/null
fi
exit 0
"#,
    )
    .expect("write fake docker");
    let mut permissions = std::fs::metadata(&docker).expect("metadata").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&docker, permissions).expect("chmod");
    let existing_path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![bin.clone()];
    paths.extend(std::env::split_paths(&existing_path));
    let path = std::env::join_paths(paths).expect("join PATH");

    let output = Command::new(env!("CARGO_BIN_EXE_jiji"))
        .arg("deploy")
        .arg("-c")
        .arg(&config_path)
        .arg("--yes")
        .arg("--build")
        .arg("--version")
        .arg("v1")
        .env("PATH", path)
        .output()
        .expect("run jiji deploy");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let received = harness.received.lock().unwrap().clone();
    assert!(
        received.iter().any(|c| c
            == &remote_command(
                "docker",
                &[
                    "build",
                    "-f",
                    &format!("{staging_root}/context/web/Dockerfile"),
                    "-t",
                    "registry.example.com/demo-web:v1",
                    "-t",
                    "registry.example.com/demo-web:latest",
                    &format!("{staging_root}/context/web"),
                ]
            )),
        "expected a remote build command on the builder session: {received:?}"
    );
    assert!(
        received
            .iter()
            .any(|c| c == "docker pull registry.example.com/demo-web:v1"),
        "the deploy target should pull the image the remote builder pushed: {received:?}"
    );
    assert!(
        received
            .iter()
            .any(|c| c == &format!("rm -rf {staging_root}")),
        "the builder's staging root should be cleaned up: {received:?}"
    );
}
