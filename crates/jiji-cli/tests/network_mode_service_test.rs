//! Integration tests for `network_mode: "service:<upstream>"` (container-namespace-sharing
//! dependents), mirroring `deploy_test.rs`'s in-process mock-SSH pattern.

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
    } else if command.contains("# jiji-request:desired-commit") {
        r#"{"Ok":{"type":"desired_state","record":{"project_id":"demo","recovery_epoch":1,"protocol_version":1,"schema_version":1,"service":"gluetun","replica_override":1,"assignments":[{"replica_id":"gluetun-test","ordinal":0,"owner_node_id":"app"}],"revision":1,"author_node_id":"app","author_epoch":1}}}"#
    } else if command.contains("# jiji-request:desired-read") {
        r#"{"Ok":{"type":"desired_state","record":null}}"#
    } else if command.contains("# jiji-request:allocate-address") {
        r#"{"Ok":{"type":"address_lease","deployment_id":"test-deploy","replica_id":"gluetun-test","address":"100.64.0.10","state":"active"}}"#
    } else if command.contains("# jiji-request:catalog-commit") {
        r#"{"Ok":{"type":"catalog_committed","record":{"project_id":"demo","recovery_epoch":1,"protocol_version":1,"schema_version":2,"service":"gluetun","replica_id":"gluetun-test","owner_node_id":"app","owner_epoch":1,"revision":1,"deployment_id":"test-deploy","address":"100.64.0.10","ports":[],"image":"docker.io/qmcgaw/gluetun:latest","state":"active","health":"healthy"}}}"#
    } else if command.contains("# jiji-request:release-address") {
        r#"{"Ok":{"type":"address_released","released":true}}"#
    } else if command.contains("# jiji-request:cron-spec-list") {
        r#"{"Ok":{"type":"cron_specs","specs":[]}}"#
    } else if command.contains("# jiji-request:health") {
        return success(&format!(
            r#"{{"Ok":{{"type":"health","schema_version":1,"observation_count":0,"version":"{}"}}}}"#,
            env!("CARGO_PKG_VERSION")
        ));
    } else {
        ""
    };
    success(body)
}

/// A `CatalogList` response reporting one Active/Healthy `gluetun` record owned by `owner`, for a
/// dependent's own upstream-resolution catalog read to find.
fn gluetun_active_catalog_response(
    deployment_id: &str,
    address: &str,
    owner: &str,
) -> CannedResponse {
    success(&format!(
        r#"{{"Ok":{{"type":"catalog_list","records":[{{"project_id":"demo","recovery_epoch":1,"protocol_version":1,"schema_version":2,"service":"gluetun","replica_id":"gluetun-existing","owner_node_id":"{owner}","owner_epoch":1,"revision":2,"deployment_id":"{deployment_id}","address":"{address}","ports":[],"image":"docker.io/qmcgaw/gluetun:latest","state":"active","health":"healthy"}}]}}}}"#
    ))
}

/// A `CatalogList` response reporting the same Active/Healthy `gluetun` record as
/// `gluetun_active_catalog_response`, plus one stuck `Draining` record for an older `gluetun`
/// deployment (simulating an earlier redeploy whose old container couldn't be removed because
/// this exact dependent was still attached to it -- see `sweep_stuck_draining_records`).
fn gluetun_active_with_stuck_draining_catalog_response(
    active_deployment_id: &str,
    active_address: &str,
    stuck_deployment_id: &str,
    stuck_address: &str,
    owner: &str,
) -> CannedResponse {
    success(&format!(
        r#"{{"Ok":{{"type":"catalog_list","records":[{{"project_id":"demo","recovery_epoch":1,"protocol_version":1,"schema_version":2,"service":"gluetun","replica_id":"gluetun-existing","owner_node_id":"{owner}","owner_epoch":1,"revision":2,"deployment_id":"{active_deployment_id}","address":"{active_address}","ports":[],"image":"docker.io/qmcgaw/gluetun:latest","state":"active","health":"healthy"}},{{"project_id":"demo","recovery_epoch":1,"protocol_version":1,"schema_version":2,"service":"gluetun","replica_id":"gluetun-existing","owner_node_id":"{owner}","owner_epoch":1,"revision":1,"deployment_id":"{stuck_deployment_id}","address":"{stuck_address}","ports":[],"image":"docker.io/qmcgaw/gluetun:latest","state":"draining","health":"unknown"}}]}}}}"#
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

/// `gluetun` (upstream, `network_mode: bridge`) and `qbittorrent` (dependent,
/// `network_mode: service:gluetun`), both on a single server.
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
  gluetun:
    image: qmcgaw/gluetun:latest
    servers: [app]
  qbittorrent:
    image: lscr.io/linuxserver/qbittorrent:latest
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

fn write_config(
    dir: &std::path::Path,
    addr: SocketAddr,
    key_path: &std::path::Path,
) -> std::path::PathBuf {
    let config_path = dir.join("deploy.yml");
    std::fs::write(&config_path, config_yaml(addr, key_path)).expect("write test deploy.yml");
    config_path
}

fn plan_generation(addr: SocketAddr) -> String {
    let yaml = config_yaml(addr, std::path::Path::new("/dev/null"));
    let config: Config = serde_yaml::from_str(&yaml).expect("parse test config");
    NetworkPlanner::new()
        .plan(&config)
        .expect("build test plan")
        .mesh_generation
}

fn run_jiji_deploy(config_path: &std::path::Path, extra_args: &[&str]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_jiji"));
    command
        .arg("deploy")
        .arg("-c")
        .arg(config_path)
        .arg("--yes");
    for arg in extra_args {
        command.arg(arg);
    }
    command.output().expect("run jiji deploy")
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

fn slug() -> String {
    jiji_network::systemd_unit_slug("demo")
}

fn network_dir() -> String {
    format!("/etc/jiji/network/{}", slug())
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

fn image_inspect_command(image: &str) -> String {
    format!("docker image inspect {image} >/dev/null 2>&1")
}

fn base_responses(generation: &str) -> HashMap<String, CannedResponse> {
    let mut responses = HashMap::new();
    responses.insert(generation_path(), success(&format!("{generation}\n")));
    responses.insert(
        service_runtime_generation_path(),
        success(&format!("{generation}\n")),
    );
    responses.insert(
        image_inspect_command("docker.io/qmcgaw/gluetun:latest"),
        success(""),
    );
    responses.insert(
        image_inspect_command("docker.io/linuxserver/qbittorrent:latest"),
        success(""),
    );
    responses.insert(mktemp_command(), cutover_generation_path("abc123"));
    responses
}

#[tokio::test(flavor = "multi_thread")]
async fn dependent_alone_attaches_to_the_existing_upstream_without_its_own_address() {
    let (dir, key_path, client_key) = setup_test_dir();
    let generation = plan_generation(SocketAddr::from(([127, 0, 0, 1], 0)));
    let mut responses = base_responses(&generation);
    // The exact agent-request command path is project-slug-derived and identical across every
    // catalog-list call in this test (CatalogList has no parameters), so one override covers it.
    let catalog_list_command = format!(
        "/etc/jiji/agent/{}/bin/jiji-agent request --socket /etc/jiji/agent/{}/agent.sock # jiji-request:catalog-list",
        slug(), slug()
    );
    responses.insert(
        catalog_list_command,
        gluetun_active_catalog_response("aabbccddeeff001122", "100.64.0.20", "app"),
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);

    let output = run_jiji_deploy(&config_path, &["-S", "qbittorrent"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");

    let received = harness.received.lock().unwrap().clone();
    assert!(
        received.iter().any(|c| {
            c.contains("docker run --name demo-qbittorrent-")
                && c.contains("--network container:demo-gluetun-aabbccddeeff")
                && !c.contains("--ip")
        }),
        "qbittorrent should join gluetun's namespace without its own address: {received:?}"
    );
    assert!(
        !received
            .iter()
            .any(|c| c.contains("docker run --name demo-gluetun-")),
        "gluetun must never be touched when only the dependent is selected: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn dependent_alone_fails_actionably_without_an_active_upstream() {
    let (dir, key_path, client_key) = setup_test_dir();
    let generation = plan_generation(SocketAddr::from(([127, 0, 0, 1], 0)));
    let responses = base_responses(&generation);
    // No catalog-list override: falls through to `default_response`'s empty record list, so
    // gluetun has no Active/Healthy record for qbittorrent to resolve.

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);

    let output = run_jiji_deploy(&config_path, &["-S", "qbittorrent"]);
    assert!(!output.status.success(), "expected the deploy to fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("gluetun") && stderr.contains("Deploy 'gluetun' first"),
        "stderr: {stderr}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn selecting_the_upstream_cascades_and_sequences_its_dependent() {
    let (dir, key_path, client_key) = setup_test_dir();
    let generation = plan_generation(SocketAddr::from(([127, 0, 0, 1], 0)));
    let mut responses = base_responses(&generation);
    let catalog_list_command = format!(
        "/etc/jiji/agent/{}/bin/jiji-agent request --socket /etc/jiji/agent/{}/agent.sock # jiji-request:catalog-list",
        slug(), slug()
    );
    // Occurrence #1: gluetun's own "is there a previous deployment" check -- first-ever deploy,
    // nothing yet. Occurrence #2: gluetun's own post-deploy sweep for stuck `Draining` records left
    // by an earlier redeploy blocked on a dependent (see `sweep_stuck_draining_records`) -- also
    // empty here, nothing stuck. Occurrence #3: qbittorrent's own upstream-resolution read, once
    // gluetun's own deploy has completed and released this same wave -- reports gluetun as active
    // (a fixed placeholder deployment_id/address, not gluetun's real freshly-generated one, since
    // this mock harness serves static canned responses rather than a truly evolving catalog).
    responses.insert(
        format!("{catalog_list_command}#1"),
        default_response(&catalog_list_command),
    );
    responses.insert(
        format!("{catalog_list_command}#2"),
        default_response(&catalog_list_command),
    );
    responses.insert(
        format!("{catalog_list_command}#3"),
        gluetun_active_catalog_response("aabbccddeeff001122", "100.64.0.20", "app"),
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);

    // Only gluetun is explicitly selected; qbittorrent must be cascaded in automatically.
    let output = run_jiji_deploy(&config_path, &["-S", "gluetun"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("demo:qbittorrent:"),
        "the cascaded dependent should appear in the deploy plan/results: {stdout}"
    );

    let received = harness.received.lock().unwrap().clone();
    let gluetun_create = received
        .iter()
        .position(|c| c.contains("docker run --name demo-gluetun-"));
    let qbittorrent_create = received
        .iter()
        .position(|c| c.contains("docker run --name demo-qbittorrent-"));
    assert!(
        gluetun_create.is_some() && qbittorrent_create.is_some(),
        "both the upstream and its cascaded dependent should have been created: {received:?}"
    );
    assert!(
        gluetun_create.unwrap() < qbittorrent_create.unwrap(),
        "gluetun must be created before qbittorrent attaches to its namespace: {received:?}"
    );
    assert!(
        received.iter().any(|c| {
            c.contains("docker run --name demo-qbittorrent-")
                && c.contains("--network container:demo-gluetun-")
                && !c.contains("--ip")
        }),
        "qbittorrent should join gluetun's own newly-created container: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn dependent_redeploy_sweeps_a_stuck_draining_record_for_its_upstream() {
    let (dir, key_path, client_key) = setup_test_dir();
    let generation = plan_generation(SocketAddr::from(([127, 0, 0, 1], 0)));
    let mut responses = base_responses(&generation);
    let catalog_list_command = format!(
        "/etc/jiji/agent/{}/bin/jiji-agent request --socket /etc/jiji/agent/{}/agent.sock # jiji-request:catalog-list",
        slug(),
        slug()
    );
    // The stuck deployment left a container named "demo-gluetun-<first 12 hex chars>" -- deploy_id
    // chosen so that prefix is easy to assert on below.
    responses.insert(
        catalog_list_command,
        gluetun_active_with_stuck_draining_catalog_response(
            "aabbccddeeff001122",
            "100.64.0.20",
            "gluetunstuck0000001",
            "100.64.0.21",
            "app",
        ),
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);

    let output = run_jiji_deploy(&config_path, &["-S", "qbittorrent"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");

    let received = harness.received.lock().unwrap().clone();
    assert!(
        received
            .iter()
            .any(|c| c.contains("docker rm -f demo-gluetun-gluetunstuck")),
        "the sweep should have removed the stuck previous gluetun container: {received:?}"
    );
    // The exec command text never carries the idempotency key or request body (those travel only
    // over stdin, which this mock harness doesn't capture) -- so the release/tombstone calls
    // triggered by the sweep can't be matched by deployment-id substring. Instead, count calls:
    // qbittorrent itself is a first-time dependent deploy with no previous record of its own (no
    // lease ever allocated, so no release-address call, and exactly two catalog-commit calls for
    // its own Candidate then Active states), so any release-address call, and any catalog-commit
    // call beyond those two, must be the sweep acting on gluetun's stuck `Draining` record.
    let release_address_calls = received
        .iter()
        .filter(|c| c.contains("# jiji-request:release-address"))
        .count();
    assert_eq!(
        release_address_calls, 1,
        "the sweep should have released exactly the stuck deployment's address lease \
         (qbittorrent itself never allocates one): {received:?}"
    );
    let catalog_commit_calls = received
        .iter()
        .filter(|c| c.contains("# jiji-request:catalog-commit"))
        .count();
    assert_eq!(
        catalog_commit_calls, 3,
        "expected qbittorrent's own Candidate+Active commits plus one Tombstoned commit \
         from the sweep: {received:?}"
    );
}
