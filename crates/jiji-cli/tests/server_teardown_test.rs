//! Integration tests for `jiji server teardown`, run as a real subprocess against a real,
//! in-process SSH server (mirroring `deploy_test.rs`/`server_setup_test.rs`'s pattern), so the
//! full config-load -> discover -> confirm -> per-host teardown path is exercised without
//! touching real hosts.

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
    received: Arc<Mutex<Vec<String>>>,
}

async fn spawn_test_server(
    authorized_key: PublicKey,
    responses: HashMap<String, CannedResponse>,
) -> (Harness, SocketAddr) {
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

    (Harness { received }, addr)
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
    servers: [app]
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

/// Two servers: "app" (reachable, hosts "web") and "data" (unreachable, no services -- teardown
/// targets every configured server regardless of service placement).
fn config_yaml_two_servers(
    reachable: SocketAddr,
    unreachable: SocketAddr,
    key_path: &std::path::Path,
    engine: &str,
) -> String {
    format!(
        r#"
project: demo
builder: {{ engine: {engine} }}
servers:
  app:
    host: {reachable_ip}
    port: {reachable_port}
    keys:
      - {key_path}
  data:
    host: {unreachable_ip}
    port: {unreachable_port}
    keys:
      - {key_path}
services:
  web:
    image: example/web:latest
    servers: [app]
ssh:
  user: tester
  keys_only: true
  connect_timeout: 2
"#,
        reachable_ip = reachable.ip(),
        reachable_port = reachable.port(),
        unreachable_ip = unreachable.ip(),
        unreachable_port = unreachable.port(),
        key_path = key_path.display(),
    )
}

fn write_config_str(dir: &std::path::Path, contents: &str) -> std::path::PathBuf {
    let config_path = dir.join("deploy.yml");
    std::fs::write(&config_path, contents).expect("write test deploy.yml");
    config_path
}

fn run_jiji_teardown(config_path: &std::path::Path, extra_args: &[&str]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_jiji"));
    command
        .arg("server")
        .arg("teardown")
        .arg("-c")
        .arg(config_path);
    for arg in extra_args {
        command.arg(arg);
    }
    command.output().expect("run jiji server teardown")
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

fn inspect_status_command(engine: &str, name: &str) -> String {
    format!("{engine} inspect {name} --format '{{{{.State.Status}}}}'")
}

fn remove_container_command(engine: &str, name: &str) -> String {
    format!("{engine} rm -f {name}")
}

fn list_managed_containers_command(engine: &str, project: &str) -> String {
    format!(
        "{engine} ps -a --filter label=jiji.managed=true --filter label=jiji.project={project} --format '{{{{.Names}}}}|{{{{.Label \"jiji.project\"}}}}|{{{{.Label \"jiji.service\"}}}}|{{{{.Label \"jiji.server\"}}}}|{{{{.State}}}}'"
    )
}

fn network_attachment_count_command(engine: &str, network: &str) -> String {
    format!("{engine} ps -a --filter network={network} --format '{{{{.Names}}}}'")
}

fn network_rm_command(engine: &str, name: &str) -> String {
    format!("{engine} network rm {name}")
}

fn network_rm_force_command(engine: &str, name: &str) -> String {
    format!("{engine} network rm --force {name}")
}

fn list_other_project_containers_command(engine: &str) -> String {
    format!(
        "{engine} ps -a --filter label=jiji.managed=true --format '{{{{.Names}}}}|{{{{.Label \"jiji.project\"}}}}|{{{{.Label \"jiji.service\"}}}}|{{{{.Label \"jiji.server\"}}}}|{{{{.State}}}}'"
    )
}

/// Sets up the two canned responses shared by every "one healthy container on 'app'" scenario:
/// the label-filtered listing that discovers it, and the inspect that reports it running.
fn one_container_responses(engine: &str) -> HashMap<String, CannedResponse> {
    let mut responses = HashMap::new();
    responses.insert(
        list_managed_containers_command(engine, "demo"),
        success("demo-web-a|demo|web|app|running\n"),
    );
    responses.insert(
        inspect_status_command(engine, "demo-web-a"),
        success("running\n"),
    );
    responses
}

#[tokio::test(flavor = "multi_thread")]
async fn full_successful_teardown_reports_fully_torn_down() {
    let (dir, key_path, client_key) = setup_test_dir();
    let responses = one_container_responses("docker");
    let (harness, addr) = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config_str(dir.path(), &config_yaml(addr, &key_path, "docker"));

    let output = run_jiji_teardown(&config_path, &["-y"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "expected success, stdout: {stdout} stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("fully torn down"), "stdout: {stdout}");

    let received = harness.received.lock().unwrap().clone();
    assert!(
        received.contains(&remove_container_command("docker", "demo-web-a")),
        "the discovered container should have been removed: {received:?}"
    );
    assert!(
        received
            .iter()
            .any(|c| c.contains("rm -rf /etc/jiji/network")),
        "compiled network state should have been removed: {received:?}"
    );
    assert!(
        received
            .iter()
            .any(|c| c.contains("rm -rf /etc/jiji/certs")),
        "jiji-proxy's now-orphaned certs directory should have been removed: {received:?}"
    );
    assert!(
        received.iter().any(|c| c.contains("rm -rf .jiji/demo")),
        "the project's staged env/mount directory should have been removed: {received:?}"
    );
    assert!(
        received
            .iter()
            .any(|c| c.contains("cat >> .jiji/demo/audit.log")),
        "a successful teardown should still append a final audit entry, recreating the staging \
         directory it just removed with nothing but this one record: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn removes_the_jiji_agent_scoped_to_this_project() {
    let (dir, key_path, client_key) = setup_test_dir();
    let responses = one_container_responses("docker");
    let (harness, addr) = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config_str(dir.path(), &config_yaml(addr, &key_path, "docker"));

    let output = run_jiji_teardown(&config_path, &["-y"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let paths = AgentPaths::default_for_project("demo");
    let received = harness.received.lock().unwrap().clone();
    assert!(
        received.contains(&format!("systemctl is-active --quiet {}", paths.unit_name)),
        "teardown should probe the agent unit before removing it: {received:?}"
    );
    assert!(
        received.iter().any(|c| c
            .contains(&format!("systemctl disable --now {}", paths.unit_name))
            && c.contains(&format!("rm -f {}", paths.unit_path.display()))
            && c.contains(&format!("rm -rf {}", paths.project_dir.display()))),
        "the agent's unit, unit file, and project directory should all be removed: {received:?}"
    );

    // Regression guard, mirroring `network_teardown.rs`'s equivalent: the removal command is
    // anchored to this project's own derived paths, never a sibling project's.
    let other_paths = AgentPaths::default_for_project("some-other-project");
    assert!(
        !received
            .iter()
            .any(|c| c.contains(&other_paths.project_dir.display().to_string())),
        "teardown must never reference another project's agent directory: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn dry_run_sends_zero_mutating_commands() {
    let (dir, key_path, client_key) = setup_test_dir();
    let responses = one_container_responses("docker");
    let (harness, addr) = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config_str(dir.path(), &config_yaml(addr, &key_path, "docker"));

    let output = run_jiji_teardown(&config_path, &["--dry-run"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "expected success, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("Dry run"), "stdout: {stdout}");

    let received = harness.received.lock().unwrap().clone();
    let mutating_substrings = [
        "rm -f",
        "rm -rf",
        "rmi ",
        "volume rm",
        "network rm",
        "systemctl disable",
        "nft delete",
    ];
    for command in &received {
        for marker in mutating_substrings {
            assert!(
                !command.contains(marker),
                "dry run must never send a mutating command, found: {command}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn container_removal_failure_prevents_network_removal() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = one_container_responses("docker");
    responses.insert(remove_container_command("docker", "demo-web-a"), failure());
    let (harness, addr) = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config_str(dir.path(), &config_yaml(addr, &key_path, "docker"));

    let output = run_jiji_teardown(&config_path, &["-y"]);
    assert!(!output.status.success(), "expected non-zero exit");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("partially torn down"),
        "stderr: {stderr} stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    let received = harness.received.lock().unwrap().clone();
    assert!(
        !received
            .iter()
            .any(|c| c.contains("rm -rf /etc/jiji/network")),
        "network layer must not be torn down after an application-layer failure: {received:?}"
    );
    // Scoped to this project's own network-layer restore units (`jiji-network-restore-`,
    // `jiji-service-nat-`, `jiji-dns-`), not jiji-proxy's own shared teardown (which legitimately
    // still runs after an application-layer failure, per the ordering above, and disables its own
    // unrelated `jiji-proxy-ingress-restore.service` unit as part of removing the container it
    // belongs to).
    assert!(
        !received.iter().any(|c| c.contains("systemctl disable")
            && (c.contains("jiji-network-restore-")
                || c.contains("jiji-service-nat-")
                || c.contains("jiji-dns-"))),
        "this project's network units must not be touched after an application-layer failure: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn another_projects_container_is_left_untouched_not_blocking() {
    // Once the network layer is per-project isolated, another project's containers on the same
    // host is the normal, expected case (see the network-isolation design notes) -- teardown must
    // proceed and remove only this project's own resources, surfacing the other project's
    // presence as an informational notice rather than refusing to run.
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert(
        list_managed_containers_command("docker", "demo"),
        success(""),
    );
    responses.insert(
        list_other_project_containers_command("docker"),
        success("other-web-a|other|web|app|running\n"),
    );
    responses.insert(
        format!(
            "docker network disconnect {} jiji-proxy",
            jiji_network::bridge_network_name("demo")
        ),
        CannedResponse {
            success: false,
            stdout: String::new(),
            stderr: "container is not connected to the network".to_string(),
        },
    );
    let (harness, addr) = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config_str(dir.path(), &config_yaml(addr, &key_path, "docker"));

    let output = run_jiji_teardown(&config_path, &["-y"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "expected success, stdout: {stdout} stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!stdout.contains("blocked"), "stdout: {stdout}");
    assert!(
        stdout.contains("other-web-a") && stdout.contains("left untouched"),
        "expected an informational notice about the other project's container, stdout: {stdout}"
    );

    let received = harness.received.lock().unwrap().clone();
    assert!(
        !received.iter().any(|c| c.contains("other-web-a")),
        "another project's container must never be named in a remote command: {received:?}"
    );
    assert!(
        received
            .iter()
            .any(|c| c.contains("rm -rf /etc/jiji/network")),
        "this project's own compiled network state should still be removed: {received:?}"
    );
    assert!(
        received
            .iter()
            .any(|command| command.starts_with("docker inspect --format")
                && command.contains("jiji-proxy")),
        "ingress must be refreshed even when this project's bridge was already detached: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn network_removal_retries_with_force_when_podman_reports_a_stale_attachment() {
    // Confirmed live against a real casa-lab teardown: Podman can report a bridge as having
    // "associated containers" immediately after that bridge's last container was force-removed
    // earlier in this same teardown run -- its own network backend's cleanup lags the container
    // removal by a beat. Teardown's own attachment precondition (`network_attachment_count`,
    // canned as zero here) has already confirmed nothing real is left, so a plain `network rm`
    // failing this way must be retried with `--force` and still report the network as removed,
    // not surface a spurious failure to the operator.
    let (dir, key_path, client_key) = setup_test_dir();
    let bridge = jiji_network::bridge_network_name("demo");
    let mut responses = HashMap::new();
    responses.insert(
        list_managed_containers_command("podman", "demo"),
        success(""),
    );
    responses.insert(
        format!("podman network inspect {bridge} >/dev/null 2>&1"),
        success(""),
    );
    responses.insert(
        network_attachment_count_command("podman", &bridge),
        success(""),
    );
    responses.insert(
        network_rm_command("podman", &bridge),
        CannedResponse {
            success: false,
            stdout: String::new(),
            stderr: format!(
                "\"{bridge}\" has associated containers with it. Use -f to forcibly delete containers and pods: network is being used"
            ),
        },
    );
    responses.insert(network_rm_force_command("podman", &bridge), success(""));
    let (harness, addr) = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config_str(dir.path(), &config_yaml(addr, &key_path, "podman"));

    let output = run_jiji_teardown(&config_path, &["-y"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "expected success, stdout: {stdout} stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("jiji bridge network: removed"),
        "stdout: {stdout}"
    );

    let received = harness.received.lock().unwrap().clone();
    assert!(
        received.contains(&network_rm_command("podman", &bridge)),
        "the plain removal should have been attempted first: {received:?}"
    );
    assert!(
        received.contains(&network_rm_force_command("podman", &bridge)),
        "the forced retry should have run after the plain removal reported a stale attachment: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stopped_jiji_proxy_is_treated_as_having_no_routes_instead_of_failing_discovery() {
    // Confirmed live: a jiji-proxy container that exists but isn't running (state "exited",
    // "created", etc.) can't be exec'ed into -- `podman exec`/`docker exec` refuse with "can only
    // create exec sessions on running containers". Before this fix, `list_routes` only checked
    // whether the container existed at all, so it still tried to exec `jiji-proxy list` against a
    // present-but-stopped container and surfaced that engine error as a hard discovery failure,
    // making teardown treat the whole host as unreachable. A stopped proxy is by definition
    // serving nothing, so this must be treated the same as "no routes" and teardown must still
    // proceed to remove it.
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert(
        list_managed_containers_command("podman", "demo"),
        success(""),
    );
    responses.insert(
        inspect_status_command("podman", "jiji-proxy"),
        success("exited\n"),
    );
    responses.insert(
        "podman exec --no-session jiji-proxy jiji-proxy list".to_string(),
        CannedResponse {
            success: false,
            stdout: String::new(),
            stderr: "Error: can only create exec sessions on running containers: container state improper".to_string(),
        },
    );
    let (harness, addr) = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config_str(dir.path(), &config_yaml(addr, &key_path, "podman"));

    let output = run_jiji_teardown(&config_path, &["-y"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "expected success, stdout: {stdout} stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !stdout.contains("could not discover"),
        "a stopped jiji-proxy must not fail discovery: stdout: {stdout}"
    );
    assert!(
        stdout.contains("jiji-proxy container: removed"),
        "the stopped jiji-proxy container should still be force-removed: stdout: {stdout}"
    );

    let received = harness.received.lock().unwrap().clone();
    assert!(
        !received.contains(&"podman exec --no-session jiji-proxy jiji-proxy list".to_string()),
        "a stopped jiji-proxy must never be exec'ed into: {received:?}"
    );
    assert!(
        received.contains(&remove_container_command("podman", "jiji-proxy")),
        "the stopped jiji-proxy container should still be removed via force-remove: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn one_unreachable_host_does_not_hide_a_successful_host() {
    let (dir, key_path, client_key) = setup_test_dir();
    let responses = one_container_responses("docker");
    let (harness, reachable_addr) =
        spawn_test_server(client_key.public_key().clone(), responses).await;

    // Bind and immediately drop, to get a port nothing is listening on.
    let unreachable_addr = {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind throwaway listener");
        listener.local_addr().expect("read listener addr")
    };

    let config_path = write_config_str(
        dir.path(),
        &config_yaml_two_servers(reachable_addr, unreachable_addr, &key_path, "docker"),
    );

    let output = run_jiji_teardown(&config_path, &["-y"]);
    assert!(!output.status.success(), "expected non-zero exit overall");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.contains("fully torn down"), "stdout: {stdout}");
    assert!(
        stderr.contains("unreachable") || stdout.contains("unreachable"),
        "stdout: {stdout} stderr: {stderr}"
    );

    let received = harness.received.lock().unwrap().clone();
    assert!(
        received.contains(&remove_container_command("docker", "demo-web-a")),
        "the reachable host's container should still have been removed: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn services_flag_is_rejected_before_any_ssh_command() {
    let (dir, key_path, client_key) = setup_test_dir();
    let (harness, addr) = spawn_test_server(client_key.public_key().clone(), HashMap::new()).await;
    let config_path = write_config_str(dir.path(), &config_yaml(addr, &key_path, "docker"));

    let mut command = Command::new(env!("CARGO_BIN_EXE_jiji"));
    command
        .arg("-S")
        .arg("web")
        .arg("server")
        .arg("teardown")
        .arg("-c")
        .arg(&config_path)
        .arg("-y");
    let output = command.output().expect("run jiji server teardown -S web");

    assert!(!output.status.success(), "expected non-zero exit");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("does not accept"), "stderr: {stderr}");

    let received = harness.received.lock().unwrap().clone();
    assert!(
        received.is_empty(),
        "expected zero SSH commands: {received:?}"
    );
}
