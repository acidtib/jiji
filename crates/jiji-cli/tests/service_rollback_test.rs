//! Integration tests for `jiji service rollback`, run as a real subprocess against a real,
//! in-process SSH server (mirroring `service_restart_test.rs`'s pattern, since rollback is built
//! directly on the same `deploy_endpoint` primitive as `jiji deploy`/`jiji service restart`, just
//! with the target image resolved from `--version` instead of whatever is currently running).

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

fn default_response(command: &str) -> CannedResponse {
    let body = if command.contains("# jiji-request:catalog-list") {
        r#"{"Ok":{"type":"catalog_list","records":[]}}"#
    } else if command.contains("# jiji-request:allocate-address") {
        r#"{"Ok":{"type":"address_lease","deployment_id":"test-deploy","replica_id":"web-test","address":"100.64.0.10","state":"active"}}"#
    } else if command.contains("# jiji-request:catalog-commit") {
        r#"{"Ok":{"type":"catalog_committed","record":{"project_id":"demo","recovery_epoch":1,"protocol_version":1,"schema_version":2,"service":"web","replica_id":"web-test","owner_node_id":"node-test","owner_epoch":1,"revision":1,"deployment_id":"test-deploy","address":"100.64.0.10","ports":[],"image":"docker.io/example/web:latest","state":"active","health":"healthy"}}}"#
    } else if command.contains("# jiji-request:release-address") {
        r#"{"Ok":{"type":"address_released","released":true}}"#
    } else if command.contains("# jiji-request:cron-spec-list") {
        r#"{"Ok":{"type":"cron_specs","specs":[]}}"#
    } else {
        ""
    };
    success(body)
}

fn agent_catalog_command() -> String {
    "/etc/jiji/agent/demo-354b6884/bin/jiji-agent request --socket \
     /etc/jiji/agent/demo-354b6884/agent.sock # jiji-request:catalog-list"
        .to_string()
}

fn active_catalog_response() -> CannedResponse {
    success(
        r#"{"Ok":{"type":"catalog_list","records":[{"project_id":"demo","recovery_epoch":1,"protocol_version":1,"schema_version":2,"service":"web","replica_id":"web-c1fe97ed0787","owner_node_id":"node-test","owner_epoch":1,"revision":2,"deployment_id":"olddeployment1234567890","address":"100.64.0.9","ports":[],"image":"docker.io/example/web:latest","state":"active","health":"healthy"}]}}"#,
    )
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
        Ok(if &self.authorized_key == key {
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

/// A single service ("web", untagged image "example/web") on a single server ("app") -- untagged
/// so `--version` has a tag to apply (a service whose `image:` already carries an explicit tag has
/// nothing for rollback to roll back to, and is rejected the same way `jiji deploy --version` is).
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
services:
  web:
    image: example/web
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

/// A build-only service (no static `image:`) -- rollback must resolve the versioned tag purely
/// from `builder.registry` + project + service name, never by inspecting a running container.
fn config_yaml_build_only(addr: SocketAddr, key_path: &std::path::Path) -> String {
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
services:
  web:
    build: .
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

fn write_config(dir: &std::path::Path, contents: &str) -> std::path::PathBuf {
    let config_path = dir.join("deploy.yml");
    std::fs::write(&config_path, contents).expect("write test deploy.yml");
    config_path
}

fn plan_generation(addr: SocketAddr) -> String {
    let yaml = config_yaml(addr, std::path::Path::new("/dev/null"));
    let config: Config = serde_yaml::from_str(&yaml).expect("parse test config");
    let plan = NetworkPlanner::new()
        .plan(&config)
        .expect("build test plan");
    plan.mesh_generation
}

fn service_runtime_generation(addr: SocketAddr) -> String {
    let yaml = config_yaml(addr, std::path::Path::new("/dev/null"));
    let config: Config = serde_yaml::from_str(&yaml).expect("parse test config");
    NetworkPlanner::new()
        .plan(&config)
        .expect("build test plan")
        .mesh_generation
}

fn run_jiji_service_rollback(config_path: &std::path::Path, version: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_jiji"))
        .arg("service")
        .arg("rollback")
        .arg("--version")
        .arg(version)
        .arg("-c")
        .arg(config_path)
        .output()
        .expect("run jiji service rollback")
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

fn network_dir() -> String {
    format!(
        "/etc/jiji/network/{}",
        jiji_network::systemd_unit_slug("demo")
    )
}

fn active_slots_path() -> String {
    format!("cat {}/service-nat-current/active-slots", network_dir())
}

fn generation_path() -> String {
    format!("cat {}/mesh-generation 2>/dev/null || true", network_dir())
}

fn service_runtime_generation_path() -> String {
    format!(
        "cat {}/service-runtime-generation 2>/dev/null || true",
        network_dir()
    )
}

fn mktemp_command() -> String {
    format!(
        "mktemp -d {}/service-nat-generations/cutover.XXXXXX",
        network_dir()
    )
}

fn cutover_generation_path(suffix: &str) -> CannedResponse {
    success(&format!(
        "{}/service-nat-generations/cutover.{suffix}\n",
        network_dir()
    ))
}

fn inspect_status_command(name: &str) -> String {
    format!("docker inspect {name} --format '{{{{.State.Status}}}}'")
}

fn image_inspect_command(image: &str) -> String {
    format!("docker image inspect {image} >/dev/null 2>&1")
}

#[tokio::test(flavor = "multi_thread")]
async fn rollback_deploys_the_requested_version_of_a_statically_imaged_service() {
    let (dir, key_path, client_key) = setup_test_dir();
    let generation = plan_generation(SocketAddr::from(([127, 0, 0, 1], 0)));

    let old_name = "demo-web-olddeploymen";
    let target_image = "docker.io/example/web:v1.2.3";
    let mut responses = HashMap::new();
    responses.insert(generation_path(), success(&format!("{generation}\n")));
    responses.insert(
        service_runtime_generation_path(),
        success(&format!(
            "{}\n",
            service_runtime_generation(SocketAddr::from(([127, 0, 0, 1], 0)))
        )),
    );
    responses.insert(active_slots_path(), success("demo:web:app=a\n"));
    responses.insert(inspect_status_command(old_name), success("running\n"));
    responses.insert(image_inspect_command(target_image), success(""));
    responses.insert(agent_catalog_command(), active_catalog_response());
    responses.insert(mktemp_command(), cutover_generation_path("abc123"));

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), &config_yaml(harness.addr, &key_path));

    let output = run_jiji_service_rollback(&config_path, "v1.2.3");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(stdout.contains(target_image), "stdout: {stdout}");
    assert!(
        stdout.contains("demo:web:app: rolled back to 'v1.2.3' ("),
        "stdout: {stdout}"
    );

    let received = harness.received.lock().unwrap().clone();
    assert!(
        received.iter().any(|c| c.contains("docker run")
            && c.contains("--name demo-web-")
            && c.contains(target_image)),
        "candidate should have been created from the requested version, not the currently running \
         image: {received:?}"
    );
    assert!(
        received.contains(&format!("docker rm -f {old_name}")),
        "old slot should have been removed: {received:?}"
    );
    assert!(
        received
            .iter()
            .any(|c| c.contains("cat >> .jiji/demo/audit.log")),
        "a successful rollback should append an audit entry: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rollback_resolves_a_build_only_service_from_the_registry_without_inspecting_a_container() {
    let (dir, key_path, client_key) = setup_test_dir();
    let generation = plan_generation(SocketAddr::from(([127, 0, 0, 1], 0)));

    // Default registry is local, port 31270 -- see `jiji-config::schema::default_registry_port`.
    let target_image = "localhost:31270/demo-web:v9";
    let mut responses = HashMap::new();
    responses.insert(generation_path(), success(&format!("{generation}\n")));
    responses.insert(
        service_runtime_generation_path(),
        success(&format!(
            "{}\n",
            service_runtime_generation(SocketAddr::from(([127, 0, 0, 1], 0)))
        )),
    );
    responses.insert(active_slots_path(), success(""));
    responses.insert(image_inspect_command(target_image), success(""));
    responses.insert(mktemp_command(), cutover_generation_path("def456"));

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), &config_yaml_build_only(harness.addr, &key_path));

    let output = run_jiji_service_rollback(&config_path, "v9");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(stdout.contains(target_image), "stdout: {stdout}");

    let received = harness.received.lock().unwrap().clone();
    assert!(
        !received
            .iter()
            .any(|c| c.contains("docker inspect demo-web-a --format '{{.Config.Image}}'")),
        "rollback must never discover an image by inspecting a running container -- the whole \
         point is deploying a specific requested version: {received:?}"
    );
    assert!(
        received.iter().any(|c| c.contains("docker run")
            && c.contains("--name demo-web-")
            && c.contains(target_image)),
        "candidate should have been created from the registry-resolved version: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rollback_without_a_version_fails_actionably() {
    let (dir, key_path, client_key) = setup_test_dir();
    let harness = spawn_test_server(client_key.public_key().clone(), HashMap::new()).await;
    let config_path = write_config(dir.path(), &config_yaml(harness.addr, &key_path));

    let output = Command::new(env!("CARGO_BIN_EXE_jiji"))
        .arg("service")
        .arg("rollback")
        .arg("-c")
        .arg(&config_path)
        .output()
        .expect("run jiji service rollback");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--version"), "stderr: {stderr}");

    // No SSH round trip should have happened at all -- the missing --version check must fail
    // before connecting to any host.
    assert!(harness.received.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn rollback_rejects_a_service_whose_image_already_has_an_explicit_tag() {
    let (dir, key_path, client_key) = setup_test_dir();
    let harness = spawn_test_server(client_key.public_key().clone(), HashMap::new()).await;
    let config_path = write_config(
        dir.path(),
        &format!(
            r#"
project: demo
builder: {{ engine: docker }}
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
            ip = harness.addr.ip(),
            port = harness.addr.port(),
            key_path = key_path.display(),
        ),
    );

    let output = run_jiji_service_rollback(&config_path, "v1.2.3");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("already has an explicit tag"),
        "stderr: {stderr}"
    );
}

/// "gluetun" (upstream, `network_mode: bridge`) and "qbittorrent" (dependent, `network_mode:
/// service:gluetun`), both on a single server, both with untagged static images -- verifies `jiji
/// service rollback` cascades a `network_mode: service:<upstream>` dependent the same way `jiji
/// deploy`/`jiji service restart` do (see `crate::cascade`).
fn config_yaml_with_dependent(addr: SocketAddr, key_path: &std::path::Path) -> String {
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
services:
  gluetun:
    image: qmcgaw/gluetun
    servers: [app]
  qbittorrent:
    image: linuxserver/qbittorrent
    servers: [app]
    network_mode: service:gluetun
ssh:
  user: tester
  keys_only: true
"#,
        ip = addr.ip(),
        port = addr.port(),
        key_path = key_path.display(),
    )
}

fn generation_with_dependent(addr: SocketAddr) -> String {
    let yaml = config_yaml_with_dependent(addr, std::path::Path::new("/dev/null"));
    let config: Config = serde_yaml::from_str(&yaml).expect("parse test config");
    NetworkPlanner::new()
        .plan(&config)
        .expect("build test plan")
        .mesh_generation
}

fn run_jiji_service_rollback_with_args(
    config_path: &std::path::Path,
    version: &str,
    extra_args: &[&str],
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_jiji"));
    command
        .arg("service")
        .arg("rollback")
        .arg("--version")
        .arg(version)
        .arg("-c")
        .arg(config_path);
    for arg in extra_args {
        command.arg(arg);
    }
    command.output().expect("run jiji service rollback")
}

#[tokio::test(flavor = "multi_thread")]
async fn rolling_back_the_upstream_cascades_and_sequences_its_dependent() {
    let (dir, key_path, client_key) = setup_test_dir();
    let generation = generation_with_dependent(SocketAddr::from(([127, 0, 0, 1], 0)));

    // gluetun's own real, deterministically-derived replica_id: the same catalog record must
    // satisfy both gluetun's own "is there a previous deployment" lookup (by replica_id) and
    // qbittorrent's upstream-resolution lookup (by service+owner_node_id).
    let gluetun_replica_id = jiji_cli::placement::replica_id("demo", "gluetun", 0);
    let old_gluetun_name = "demo-gluetun-oldgluetunde";
    let catalog_response = success(&format!(
        r#"{{"Ok":{{"type":"catalog_list","records":[{{"project_id":"demo","recovery_epoch":1,"protocol_version":1,"schema_version":2,"service":"gluetun","replica_id":"{gluetun_replica_id}","owner_node_id":"app","owner_epoch":1,"revision":2,"deployment_id":"oldgluetundeploy1234567","address":"100.64.0.20","ports":[],"image":"docker.io/qmcgaw/gluetun:v1.0.0","state":"active","health":"healthy"}}]}}}}"#
    ));

    let mut responses = HashMap::new();
    responses.insert(generation_path(), success(&format!("{generation}\n")));
    responses.insert(
        service_runtime_generation_path(),
        success(&format!("{generation}\n")),
    );
    responses.insert(mktemp_command(), cutover_generation_path("abc123"));
    responses.insert(agent_catalog_command(), catalog_response);
    responses.insert(
        inspect_status_command(old_gluetun_name),
        success("running\n"),
    );
    responses.insert(
        image_inspect_command("docker.io/qmcgaw/gluetun:v1.2.3"),
        success(""),
    );
    responses.insert(
        image_inspect_command("docker.io/linuxserver/qbittorrent:v1.2.3"),
        success(""),
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(
        dir.path(),
        &config_yaml_with_dependent(harness.addr, &key_path),
    );

    // Only gluetun is explicitly selected; qbittorrent must be cascaded in automatically.
    let output = run_jiji_service_rollback_with_args(&config_path, "v1.2.3", &["-S", "gluetun"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        stdout.contains("demo:qbittorrent:"),
        "the cascaded dependent should appear in the rollback results: {stdout}"
    );

    let received = harness.received.lock().unwrap().clone();
    let gluetun_create = received
        .iter()
        .position(|c| c.contains("docker run --name demo-gluetun-") && c.contains("v1.2.3"));
    let qbittorrent_create = received
        .iter()
        .position(|c| c.contains("docker run --name demo-qbittorrent-") && c.contains("v1.2.3"));
    assert!(
        gluetun_create.is_some() && qbittorrent_create.is_some(),
        "both the upstream and its cascaded dependent should have been rolled back to v1.2.3: {received:?}"
    );
    assert!(
        gluetun_create.unwrap() < qbittorrent_create.unwrap(),
        "gluetun must be rolled back before qbittorrent attaches to its namespace: {received:?}"
    );
    assert!(
        received.iter().any(|c| {
            c.contains("docker run --name demo-qbittorrent-")
                && c.contains("--network container:demo-gluetun-")
                && !c.contains("--ip")
        }),
        "qbittorrent should join gluetun's own newly-created container: {received:?}"
    );
    assert!(
        received.contains(&format!("docker rm -f {old_gluetun_name}")),
        "gluetun's previous container should have been removed after rolling back: {received:?}"
    );
}
