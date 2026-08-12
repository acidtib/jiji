//! Integration tests for `jiji deploy`, run as a real subprocess against a real, in-process SSH
//! server (mirroring `server_setup_test.rs`'s pattern), so the full config-load -> plan ->
//! connect -> per-endpoint deploy transaction path is exercised without touching real hosts.

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

fn default_response(command: &str) -> CannedResponse {
    let body = if command.contains("# jiji-request:catalog-list") {
        r#"{"Ok":{"type":"catalog_list","records":[]}}"#
    } else if command.contains("# jiji-request:desired-commit") {
        r#"{"Ok":{"type":"desired_state","record":{"project_id":"demo","recovery_epoch":1,"protocol_version":1,"schema_version":1,"service":"web","replica_override":1,"assignments":[{"replica_id":"web-c1fe97ed0787","ordinal":0,"owner_node_id":"app"}],"revision":1,"author_node_id":"app","author_epoch":1}}}"#
    } else if command.contains("# jiji-request:desired-read") {
        r#"{"Ok":{"type":"desired_state","record":null}}"#
    } else if command.contains("# jiji-request:allocate-address") {
        r#"{"Ok":{"type":"address_lease","deployment_id":"test-deploy","replica_id":"web-test","address":"100.64.0.10","state":"active"}}"#
    } else if command.contains("# jiji-request:catalog-commit") {
        r#"{"Ok":{"type":"catalog_committed","record":{"project_id":"demo","recovery_epoch":1,"protocol_version":1,"schema_version":2,"service":"web","replica_id":"web-test","owner_node_id":"node-test","owner_epoch":1,"revision":1,"deployment_id":"test-deploy","address":"100.64.0.10","ports":[],"image":"docker.io/example/web:latest","state":"active","health":"healthy"}}}"#
    } else if command.contains("# jiji-request:release-address") {
        r#"{"Ok":{"type":"address_released","released":true}}"#
    } else if command.contains("# jiji-request:cron-spec-list") {
        r#"{"Ok":{"type":"cron_specs","specs":[]}}"#
    } else if command.contains("# jiji-request:image-retention-apply") {
        r#"{"Ok":{"type":"image_retention_applied","spec":{"service":"web","repo":"localhost:31270/demo-web","retain":3,"revision":1}}}"#
    } else if command.contains("# jiji-request:image-retention-remove") {
        r#"{"Ok":{"type":"image_retention_removed","removed":true}}"#
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

fn agent_request_command(kind: &str) -> String {
    format!(
        "/etc/jiji/agent/demo-354b6884/bin/jiji-agent request --socket \
         /etc/jiji/agent/demo-354b6884/agent.sock # jiji-request:{kind}"
    )
}

fn active_catalog_response(deployment_id: &str, address: &str) -> CannedResponse {
    success(&format!(
        r#"{{"Ok":{{"type":"catalog_list","records":[{{"project_id":"demo","recovery_epoch":1,"protocol_version":1,"schema_version":2,"service":"web","replica_id":"web-c1fe97ed0787","owner_node_id":"node-test","owner_epoch":1,"revision":2,"deployment_id":"{deployment_id}","address":"{address}","ports":[],"image":"docker.io/example/web:latest","state":"active","health":"healthy"}}]}}}}"#
    ))
}

#[derive(Clone)]
struct TestServer {
    authorized_key: PublicKey,
    responses: Arc<HashMap<String, CannedResponse>>,
    pending: Arc<Mutex<HashMap<ChannelId, String>>>,
    /// Every command received, in order -- lets tests assert on absence/ordering, not just on
    /// the final canned outcome of one command.
    received: Arc<Mutex<Vec<String>>>,
    forwards: Arc<Mutex<Vec<(String, u32)>>>,
    cancelled_forwards: Arc<Mutex<Vec<(String, u32)>>>,
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

    async fn tcpip_forward(
        &mut self,
        address: &str,
        port: &mut u32,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
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
    forwards: Arc<Mutex<Vec<(String, u32)>>>,
    cancelled_forwards: Arc<Mutex<Vec<(String, u32)>>>,
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
    let forwards = Arc::new(Mutex::new(Vec::new()));
    let cancelled_forwards = Arc::new(Mutex::new(Vec::new()));
    let mut test_server = TestServer {
        authorized_key,
        responses: Arc::new(responses),
        pending: Arc::new(Mutex::new(HashMap::new())),
        received: received.clone(),
        forwards: forwards.clone(),
        cancelled_forwards: cancelled_forwards.clone(),
    };

    tokio::spawn(async move {
        let _ = test_server.run_on_socket(config, &listener).await;
        drop(listener);
    });

    Harness {
        addr,
        received,
        forwards,
        cancelled_forwards,
    }
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

fn write_config(
    dir: &std::path::Path,
    addr: SocketAddr,
    key_path: &std::path::Path,
    engine: &str,
) -> std::path::PathBuf {
    let config_path = dir.join("deploy.yml");
    std::fs::write(&config_path, config_yaml(addr, key_path, engine))
        .expect("write test deploy.yml");
    config_path
}

/// `config_yaml` plus a single `crons:` entry on "web", for Phase 5 deployment-integration tests.
fn config_yaml_with_cron(addr: SocketAddr, key_path: &std::path::Path, engine: &str) -> String {
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
    crons:
      sync:
        schedule: "*/5 * * * *"
        command: ["echo", "hi"]
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

fn write_config_with_cron(
    dir: &std::path::Path,
    addr: SocketAddr,
    key_path: &std::path::Path,
    engine: &str,
) -> std::path::PathBuf {
    let config_path = dir.join("deploy.yml");
    std::fs::write(&config_path, config_yaml_with_cron(addr, key_path, engine))
        .expect("write test deploy.yml");
    config_path
}

/// `config_yaml` plus `build: .` on "web", for image-retention deployment-integration tests.
/// Keeps `image:` too (deploy without `--build` requires it -- `build:` alone with no `--build`
/// flag is rejected) so this only adds `services.web.build.is_some()`, matching the condition
/// `image_retention_reconcile::services_to_reconcile` actually checks.
fn config_yaml_with_build(addr: SocketAddr, key_path: &std::path::Path, engine: &str) -> String {
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
    build: .
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

fn write_config_with_build(
    dir: &std::path::Path,
    addr: SocketAddr,
    key_path: &std::path::Path,
    engine: &str,
) -> std::path::PathBuf {
    let config_path = dir.join("deploy.yml");
    std::fs::write(&config_path, config_yaml_with_build(addr, key_path, engine))
        .expect("write test deploy.yml");
    config_path
}

/// `config_yaml` plus an HTTP `proxy.healthcheck` on "web" (so `wait_until_healthy` polls a
/// deterministic `curl` command -- see `healthcheck_command` -- instead of a container-readiness
/// check keyed on a random deployment ID), for health-check-progress integration tests. `secrets`
/// becomes `environment.secrets`, resolved from the host environment (`--host-env`) rather than an
/// `.env` file, since `project_root_from_config_path` resolves two levels above the config path
/// and every other test in this file writes `deploy.yml` directly under the temp dir.
fn config_yaml_with_healthcheck(
    addr: SocketAddr,
    key_path: &std::path::Path,
    engine: &str,
    interval: &str,
    deploy_timeout: &str,
    secrets: &[&str],
) -> String {
    let secrets_yaml = if secrets.is_empty() {
        String::new()
    } else {
        let list = secrets
            .iter()
            .map(|name| format!("        - {name}"))
            .collect::<Vec<_>>()
            .join("\n");
        format!("    environment:\n      secrets:\n{list}\n")
    };
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
    proxy:
      hosts: [web.test]
      port: 3000
      healthcheck:
        path: /health
        interval: "{interval}"
        deploy_timeout: "{deploy_timeout}"
        timeout: "1s"
{secrets_yaml}ssh:
  user: tester
  keys_only: true
"#,
        engine = engine,
        ip = addr.ip(),
        port = addr.port(),
        key_path = key_path.display(),
        interval = interval,
        deploy_timeout = deploy_timeout,
        secrets_yaml = secrets_yaml,
    )
}

fn write_config_with_healthcheck(
    dir: &std::path::Path,
    addr: SocketAddr,
    key_path: &std::path::Path,
    engine: &str,
    interval: &str,
    deploy_timeout: &str,
    secrets: &[&str],
) -> std::path::PathBuf {
    let config_path = dir.join("deploy.yml");
    std::fs::write(
        &config_path,
        config_yaml_with_healthcheck(addr, key_path, engine, interval, deploy_timeout, secrets),
    )
    .expect("write test deploy.yml");
    config_path
}

/// The exact `curl` command `health_check::plan_for_candidate` renders for
/// `config_yaml_with_healthcheck`'s `proxy.healthcheck` -- deterministic because the mock agent's
/// default `# jiji-request:allocate-address` response always leases `100.64.0.10` (see
/// `default_response`), so, unlike a container-readiness check, this command never embeds the
/// random per-deployment container name.
fn healthcheck_command() -> String {
    "curl -fsS --max-time 1 http://100.64.0.10:3000/health".to_string()
}

/// Two servers: "app" hosts the only service and is reachable; "peer" is configured but
/// unreachable (port 1, nothing listens there), so `--wait-for-peers` has exactly one
/// unreachable peer to report as offline.
fn config_yaml_with_unreachable_peer(addr: SocketAddr, key_path: &std::path::Path) -> String {
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
  peer:
    host: 127.0.0.1
    port: 1
    keys:
      - {key_path}
services:
  web:
    image: example/web:latest
    servers: [app]
ssh:
  user: tester
  keys_only: true
  connect_timeout: 1
"#,
        ip = addr.ip(),
        port = addr.port(),
        key_path = key_path.display(),
    )
}

fn write_config_with_unreachable_peer(
    dir: &std::path::Path,
    addr: SocketAddr,
    key_path: &std::path::Path,
) -> std::path::PathBuf {
    let config_path = dir.join("deploy.yml");
    std::fs::write(
        &config_path,
        config_yaml_with_unreachable_peer(addr, key_path),
    )
    .expect("write test deploy.yml");
    config_path
}

fn plan_generation_with_unreachable_peer(addr: SocketAddr) -> String {
    let yaml = config_yaml_with_unreachable_peer(addr, std::path::Path::new("/dev/null"));
    let config: Config = serde_yaml::from_str(&yaml).expect("parse test config");
    NetworkPlanner::new()
        .plan(&config)
        .expect("build test plan")
        .mesh_generation
}

fn plan_generation(addr: SocketAddr, engine: &str) -> String {
    let yaml = config_yaml(addr, std::path::Path::new("/dev/null"), engine);
    let config: Config = serde_yaml::from_str(&yaml).expect("parse test config");
    let plan = NetworkPlanner::new()
        .plan(&config)
        .expect("build test plan");
    plan.mesh_generation
}

fn service_runtime_generation(addr: SocketAddr, engine: &str) -> String {
    let yaml = config_yaml(addr, std::path::Path::new("/dev/null"), engine);
    let config: Config = serde_yaml::from_str(&yaml).expect("parse test config");
    NetworkPlanner::new()
        .plan(&config)
        .expect("build test plan")
        .mesh_generation
}

fn current_service_runtime_generation(engine: &str) -> CannedResponse {
    success(&format!(
        "{}\n",
        service_runtime_generation(SocketAddr::from(([127, 0, 0, 1], 0)), engine)
    ))
}

/// Always passes `--yes`: the test subprocess has no controlling terminal, so without it every
/// deploy would bail on the new non-interactive confirmation guard before doing anything.
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

/// Every test config in this file uses `project: demo` (see `config_yaml`), so every project-scoped
/// remote path/name can be derived from that one fixed slug.
fn slug() -> String {
    jiji_network::systemd_unit_slug("demo")
}

fn network_dir() -> String {
    format!("/etc/jiji/network/{}", slug())
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

fn public_key_command() -> String {
    let dir = network_dir();
    format!("test -s {dir}/public.key && cat {dir}/public.key")
}

fn capture_generations_command() -> String {
    let dir = network_dir();
    format!(
        "set -eu; if test -L {dir}/current; then readlink -f {dir}/current; else printf '%s\\n' -; fi; if test -L {dir}/dns-current; then readlink -f {dir}/dns-current; else printf '%s\\n' -; fi"
    )
}

/// `service_network::persist_state` validates that `mktemp`'s reported path actually starts with
/// this project's `service-nat-generations/` prefix, so the canned stdout for `mktemp_command()`
/// must be project-scoped too, not just its own command key.
fn cutover_generation_path(suffix: &str) -> CannedResponse {
    success(&format!(
        "{}/service-nat-generations/cutover.{suffix}\n",
        network_dir()
    ))
}

fn inspect_status_command(engine: &str, name: &str) -> String {
    format!("{engine} inspect {name} --format '{{{{.State.Status}}}}'")
}

fn readiness_health_command(engine: &str, name: &str) -> String {
    format!("{engine} inspect {name} --format '{{{{.State.Status}}}}' | grep -qx running")
}

fn image_inspect_command(engine: &str, image: &str) -> String {
    format!("{engine} image inspect {image} >/dev/null 2>&1")
}

fn inspect_image_id_command(engine: &str, name: &str) -> String {
    format!("{engine} inspect {name} --format '{{{{.Image}}}}' 2>/dev/null || true")
}

fn referenced_elsewhere_command(engine: &str, image: &str) -> String {
    format!("{engine} ps -a --filter ancestor={image} --format '{{{{.Names}}}}'")
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_compiled_mesh_does_not_block_deploy_or_mutate_wireguard() {
    let (dir, key_path, client_key) = setup_test_dir();
    let generation = plan_generation(SocketAddr::from(([127, 0, 0, 1], 0)), "docker");
    let mut responses = HashMap::new();
    responses.insert(generation_path(), success("stale-generation\n"));
    responses.insert(
        service_runtime_generation_path(),
        current_service_runtime_generation("docker"),
    );
    responses.insert(
        format!("{}#2", generation_path()),
        success("stale-generation\n"),
    );
    responses.insert(
        format!("{}#3", generation_path()),
        success(&format!("{generation}\n")),
    );
    responses.insert("id -u".to_string(), success("0\n"));
    responses.insert(public_key_command(), success("test-wireguard-public-key\n"));
    responses.insert(capture_generations_command(), success("-\n-\n"));
    responses.insert(inspect_status_command("docker", "demo-web-a"), failure());
    responses.insert(
        image_inspect_command("docker", "docker.io/example/web:latest"),
        success(""),
    );
    responses.insert(
        readiness_health_command("docker", "demo-web-a"),
        success(""),
    );
    responses.insert(mktemp_command(), cutover_generation_path("auto123"));

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path, "docker");

    let output = run_jiji_deploy(&config_path, &[]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success(), "stderr: {stderr}");

    let received = harness.received.lock().unwrap().clone();
    assert!(
        !received.contains(&generation_path()),
        "deploy must not inspect the retired compiled mesh generation"
    );
    assert!(
        !received
            .iter()
            .any(|command| command.starts_with("wg ") || command.contains("wg-quick")),
        "deploy must not reconcile WireGuard: {received:?}"
    );
    assert!(
        received
            .iter()
            .any(|command| command.contains("run --name")),
        "agent-owned mesh state must not block service deployment: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn first_deployment_creates_the_candidate_and_removes_nothing() {
    let (dir, key_path, client_key) = setup_test_dir();
    let generation = plan_generation(SocketAddr::from(([127, 0, 0, 1], 0)), "docker");
    // Port is not part of the generation checksum's identity inputs (server host/service/project
    // are), so a throwaway address is fine for computing the expected generation string.

    let mut responses = HashMap::new();
    responses.insert(generation_path(), success(&format!("{generation}\n")));
    responses.insert(
        service_runtime_generation_path(),
        current_service_runtime_generation("docker"),
    );
    responses.insert(
        image_inspect_command("docker", "docker.io/example/web:latest"),
        success(""),
    );
    responses.insert(mktemp_command(), cutover_generation_path("abc123"));

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path, "docker");

    let output = run_jiji_deploy(&config_path, &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "expected success, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("demo:web:web-") && stdout.contains(": deployed"),
        "stdout: {stdout}"
    );

    let received = harness.received.lock().unwrap().clone();
    assert!(
        received
            .iter()
            .any(|c| c.contains("docker run --name demo-web-")
                && c.contains("jiji.catalog-managed=true")),
        "candidate should have been created: {received:?}"
    );
    assert!(
        !received
            .iter()
            .any(|c| c.contains("docker rm") || c.contains("podman rm")),
        "no container should be removed on a first deployment: {received:?}"
    );
    assert!(
        received
            .iter()
            .any(|c| c.contains("cat >> .jiji/demo/audit.log")
                && c.contains("install -d -m 0700 .jiji/demo")),
        "a successful deploy should append an audit entry: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn wait_for_peers_reports_an_unreachable_peer_as_offline_without_failing_the_deploy() {
    let (dir, key_path, client_key) = setup_test_dir();
    let generation = plan_generation_with_unreachable_peer(SocketAddr::from(([127, 0, 0, 1], 0)));

    let mut responses = HashMap::new();
    responses.insert(generation_path(), success(&format!("{generation}\n")));
    responses.insert(
        service_runtime_generation_path(),
        success(&format!("{generation}\n")),
    );
    responses.insert(
        image_inspect_command("docker", "docker.io/example/web:latest"),
        success(""),
    );
    responses.insert(mktemp_command(), cutover_generation_path("abc123"));

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config_with_unreachable_peer(dir.path(), harness.addr, &key_path);

    let output = run_jiji_deploy(&config_path, &["--wait-for-peers", "1"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        stdout.contains("Replication ack: 0/1 peer(s) confirmed")
            && stdout.contains("offline/not yet observed: peer"),
        "stdout: {stdout}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn wait_for_peers_omitted_adds_no_extra_connection() {
    let (dir, key_path, client_key) = setup_test_dir();
    let generation = plan_generation_with_unreachable_peer(SocketAddr::from(([127, 0, 0, 1], 0)));

    let mut responses = HashMap::new();
    responses.insert(generation_path(), success(&format!("{generation}\n")));
    responses.insert(
        service_runtime_generation_path(),
        success(&format!("{generation}\n")),
    );
    responses.insert(
        image_inspect_command("docker", "docker.io/example/web:latest"),
        success(""),
    );
    responses.insert(mktemp_command(), cutover_generation_path("abc123"));

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config_with_unreachable_peer(dir.path(), harness.addr, &key_path);

    // Omitting --wait-for-peers must never attempt to reach "peer": if it did, this deploy would
    // hang/fail against the deliberately unreachable port 1.
    let output = run_jiji_deploy(&config_path, &[]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("Replication ack"), "stdout: {stdout}");
}

#[tokio::test(flavor = "multi_thread")]
async fn service_scale_commits_desired_state_and_deploys_a_missing_replica() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert(
        image_inspect_command("docker", "docker.io/example/web:latest"),
        success(""),
    );
    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path, "docker");
    let output = Command::new(env!("CARGO_BIN_EXE_jiji"))
        .args([
            "-S",
            "web",
            "-c",
            config_path.to_str().unwrap(),
            "service",
            "scale",
            "--replicas",
            "1",
            "--yes",
        ])
        .output()
        .expect("run jiji service scale");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let received = harness.received.lock().unwrap();
    assert!(received
        .iter()
        .any(|command| command.contains("# jiji-request:desired-commit")));
    assert!(received
        .iter()
        .any(|command| command.contains("docker run --name demo-web-")));
}

#[tokio::test(flavor = "multi_thread")]
async fn yes_flag_prints_the_deployment_plan_and_proceeds_without_prompting() {
    let (dir, key_path, client_key) = setup_test_dir();
    let generation = plan_generation(SocketAddr::from(([127, 0, 0, 1], 0)), "docker");

    let candidate_name = "demo-web-a";
    let mut responses = HashMap::new();
    responses.insert(generation_path(), success(&format!("{generation}\n")));
    responses.insert(
        service_runtime_generation_path(),
        current_service_runtime_generation("docker"),
    );
    responses.insert(active_slots_path(), success(""));
    responses.insert(inspect_status_command("docker", candidate_name), failure());
    responses.insert(
        image_inspect_command("docker", "docker.io/example/web:latest"),
        success(""),
    );
    responses.insert(
        readiness_health_command("docker", candidate_name),
        success(""),
    );
    responses.insert(mktemp_command(), cutover_generation_path("plan123"));

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path, "docker");

    let output = run_jiji_deploy(&config_path, &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "expected success, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("Deployment Plan:"), "stdout: {stdout}");
    assert!(stdout.contains("Project: demo"), "stdout: {stdout}");
    assert!(stdout.contains("Servers: app"), "stdout: {stdout}");
    assert!(stdout.contains("Endpoints (1):"), "stdout: {stdout}");
    assert!(stdout.contains("web @ app"), "stdout: {stdout}");
    assert!(
        stdout.contains("Build: no, using configured image"),
        "stdout: {stdout}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn without_yes_and_no_terminal_deploy_refuses_to_hang_on_a_prompt() {
    let (dir, key_path, client_key) = setup_test_dir();
    let harness = spawn_test_server(client_key.public_key().clone(), HashMap::new()).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path, "docker");

    let mut command = Command::new(env!("CARGO_BIN_EXE_jiji"));
    let output = command
        .arg("deploy")
        .arg("-c")
        .arg(&config_path)
        .output()
        .expect("run jiji deploy");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success(), "stdout: {stdout}");
    assert!(stdout.contains("Deployment Plan:"), "stdout: {stdout}");
    assert!(
        stderr.contains("--yes") && stderr.contains("non-interactively"),
        "stderr: {stderr}"
    );

    // A seed-agent health check, then desired placement, are the only reads before the
    // no-TTY/no-`--yes` bail; no lock, build, or deployment mutation occurs.
    let received = harness.received.lock().unwrap();
    assert_eq!(received.len(), 2, "{received:?}");
    assert!(received[0].contains("# jiji-request:health"));
    assert!(received[1].contains("# jiji-request:desired-read"));
}

#[tokio::test(flavor = "multi_thread")]
async fn replacement_removes_the_old_container_only_after_health_and_commit_succeed() {
    let (dir, key_path, client_key) = setup_test_dir();
    let generation = plan_generation(SocketAddr::from(([127, 0, 0, 1], 0)), "docker");

    let old_deployment = "olddeployment1234567890";
    let old_name = "demo-web-olddeploymen";
    let mut responses = HashMap::new();
    responses.insert(generation_path(), success(&format!("{generation}\n")));
    responses.insert(
        service_runtime_generation_path(),
        current_service_runtime_generation("docker"),
    );
    responses.insert(
        agent_request_command("catalog-list"),
        active_catalog_response(old_deployment, "100.64.0.9"),
    );
    responses.insert(
        image_inspect_command("docker", "docker.io/example/web:latest"),
        success(""),
    );
    responses.insert(mktemp_command(), cutover_generation_path("def456"));

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path, "docker");

    let output = run_jiji_deploy(&config_path, &[]);
    assert!(
        output.status.success(),
        "expected success, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let received = harness.received.lock().unwrap().clone();
    let run_index = received
        .iter()
        .position(|c| c.contains("docker run --name demo-web-"))
        .expect("candidate should have been created");
    let remove_index = received
        .iter()
        .position(|c| c.contains(&format!("rm -f {old_name}")))
        .expect("old container should eventually be removed");
    assert!(
        run_index < remove_index,
        "candidate must be created before the old container is removed: {received:?}"
    );
    assert!(
        !received
            .iter()
            .any(|c| c.contains("rm -f demo-web-") && !c.contains(old_name)),
        "the healthy candidate itself must never be removed: {received:?}"
    );
}

/// A moving tag like `:latest` leaves its previous digest permanently dangling once the old
/// container that ran it is removed -- nothing else in this codebase prunes it (retention/
/// `jiji service prune` are build-only). `deploy_transaction.rs`'s cutover captures the old
/// container's actual resolved image ID (not its `.Config.Image` reference, which is identical
/// across every pull) before removing it, then removes that specific image if nothing else on the
/// host still references it.
#[tokio::test(flavor = "multi_thread")]
async fn replacement_removes_the_old_containers_now_orphaned_image() {
    let (dir, key_path, client_key) = setup_test_dir();
    let generation = plan_generation(SocketAddr::from(([127, 0, 0, 1], 0)), "docker");

    let old_deployment = "olddeployment1234567890";
    let old_name = "demo-web-olddeploymen";
    let old_image_id = "sha256:oldimageid000000000000000000000000000000000000000000000000000";
    let mut responses = HashMap::new();
    responses.insert(generation_path(), success(&format!("{generation}\n")));
    responses.insert(
        service_runtime_generation_path(),
        current_service_runtime_generation("docker"),
    );
    responses.insert(
        agent_request_command("catalog-list"),
        active_catalog_response(old_deployment, "100.64.0.9"),
    );
    responses.insert(
        image_inspect_command("docker", "docker.io/example/web:latest"),
        success(""),
    );
    responses.insert(mktemp_command(), cutover_generation_path("def456"));
    responses.insert(
        inspect_image_id_command("docker", old_name),
        success(&format!("{old_image_id}\n")),
    );
    responses.insert(
        referenced_elsewhere_command("docker", old_image_id),
        success(""),
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path, "docker");

    let output = run_jiji_deploy(&config_path, &[]);
    assert!(
        output.status.success(),
        "expected success, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let received = harness.received.lock().unwrap().clone();
    let remove_container_index = received
        .iter()
        .position(|c| c.contains(&format!("rm -f {old_name}")))
        .expect("old container should have been removed");
    let remove_image_index = received
        .iter()
        .position(|c| c == &format!("docker rmi {old_image_id}"))
        .expect("the old container's now-orphaned image should have been removed");
    assert!(
        remove_container_index < remove_image_index,
        "the image must only be removed after the container using it is gone: {received:?}"
    );
}

/// The mirror image of `replacement_removes_the_old_containers_now_orphaned_image`: a
/// build-configured service's old image is a distinct, rollback-addressable version managed by
/// `retain:` (see `image_retention_reconcile.rs`), not something the cutover itself should ever
/// eagerly delete.
#[tokio::test(flavor = "multi_thread")]
async fn replacement_never_eagerly_removes_a_build_configured_services_old_image() {
    let (dir, key_path, client_key) = setup_test_dir();
    let generation = plan_generation(SocketAddr::from(([127, 0, 0, 1], 0)), "docker");

    let old_deployment = "olddeployment1234567890";
    let old_name = "demo-web-olddeploymen";
    let old_image_id = "sha256:oldimageid000000000000000000000000000000000000000000000000000";
    let mut responses = HashMap::new();
    responses.insert(generation_path(), success(&format!("{generation}\n")));
    responses.insert(
        service_runtime_generation_path(),
        current_service_runtime_generation("docker"),
    );
    responses.insert(
        agent_request_command("catalog-list"),
        active_catalog_response_owned_by_app(old_deployment, "100.64.0.9"),
    );
    responses.insert(
        image_inspect_command("docker", "docker.io/example/web:latest"),
        success(""),
    );
    responses.insert(mktemp_command(), cutover_generation_path("def456"));
    // Would prove the bug if this were ever queried and acted on: a build-configured service
    // must never even ask for the old container's image ID in the first place.
    responses.insert(
        inspect_image_id_command("docker", old_name),
        success(&format!("{old_image_id}\n")),
    );
    responses.insert(
        referenced_elsewhere_command("docker", old_image_id),
        success(""),
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config_with_build(dir.path(), harness.addr, &key_path, "docker");

    let output = run_jiji_deploy(&config_path, &[]);
    assert!(
        output.status.success(),
        "expected success, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let received = harness.received.lock().unwrap().clone();
    assert!(
        !received
            .iter()
            .any(|c| c == &format!("docker rmi {old_image_id}")),
        "a build-configured service's old image must be left for `retain:` to manage, not \
         eagerly removed at cutover: {received:?}"
    );
}

/// Like `active_catalog_response`, but `owner_node_id` is the real configured server name
/// ("app"), not the unrelated placeholder "node-test": cron reconciliation looks up the owner's
/// session by that name, so it must match this file's single-server test topology.
fn active_catalog_response_owned_by_app(deployment_id: &str, address: &str) -> CannedResponse {
    success(&format!(
        r#"{{"Ok":{{"type":"catalog_list","records":[{{"project_id":"demo","recovery_epoch":1,"protocol_version":1,"schema_version":2,"service":"web","replica_id":"web-c1fe97ed0787","owner_node_id":"app","owner_epoch":1,"revision":2,"deployment_id":"{deployment_id}","address":"{address}","ports":[],"image":"docker.io/example/web:latest","state":"active","health":"healthy"}}]}}}}"#
    ))
}

/// A full `type=cron_spec_applied` response for `web`'s `sync` cron: only the wire shape matters
/// for this test (parseable `ResponseBody::CronSpecApplied`), not the field values.
fn cron_spec_applied_response() -> CannedResponse {
    success(
        r#"{"Ok":{"type":"cron_spec_applied","spec":{"project":"demo","service":"web","cron_name":"sync","revision":2,"canonical_hash":"abc123","owner_node_id":"app","owner_epoch":1,"server":"app","source_deployment_id":"olddeployment1234567890","source_replica_id":"web-c1fe97ed0787","image":"docker.io/example/web:latest","schedule":"*/5 * * * *","timezone":"UTC","timeout_seconds":3600,"overlap":"forbid","missed_runs":"skip","command":["echo","hi"],"env_file_path":".jiji/demo/env/web-app.env","mount_args":[],"resource_args":[],"bridge_network":"jiji-demo","dns_address":"100.64.0.5"},"outcome":"installed"}}"#,
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn deploy_applies_cron_specs_after_catalog_activation() {
    let (dir, key_path, client_key) = setup_test_dir();
    let generation = plan_generation(SocketAddr::from(([127, 0, 0, 1], 0)), "docker");

    let old_deployment = "olddeployment1234567890";
    let mut responses = HashMap::new();
    responses.insert(generation_path(), success(&format!("{generation}\n")));
    responses.insert(
        service_runtime_generation_path(),
        current_service_runtime_generation("docker"),
    );
    responses.insert(
        agent_request_command("catalog-list"),
        active_catalog_response_owned_by_app(old_deployment, "100.64.0.9"),
    );
    responses.insert(
        image_inspect_command("docker", "docker.io/example/web:latest"),
        success(""),
    );
    responses.insert(mktemp_command(), cutover_generation_path("def456"));
    responses.insert("pwd".to_string(), success("/root"));
    responses.insert(
        agent_request_command("cron-spec-apply"),
        cron_spec_applied_response(),
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config_with_cron(dir.path(), harness.addr, &key_path, "docker");

    let output = run_jiji_deploy(&config_path, &[]);
    assert!(
        output.status.success(),
        "expected success, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let received = harness.received.lock().unwrap().clone();
    let apply_index = received
        .iter()
        .position(|c| c.contains("# jiji-request:cron-spec-apply"))
        .expect("cron spec should have been applied after deploy");
    let run_index = received
        .iter()
        .position(|c| c.contains("docker run --name demo-web-"))
        .expect("candidate should have been created");
    assert!(
        run_index < apply_index,
        "cron spec application must happen after the candidate is created/activated: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn deploy_applies_an_image_retention_spec_after_catalog_activation() {
    let (dir, key_path, client_key) = setup_test_dir();
    let generation = plan_generation(SocketAddr::from(([127, 0, 0, 1], 0)), "docker");

    let old_deployment = "olddeployment1234567890";
    let mut responses = HashMap::new();
    responses.insert(generation_path(), success(&format!("{generation}\n")));
    responses.insert(
        service_runtime_generation_path(),
        current_service_runtime_generation("docker"),
    );
    responses.insert(
        agent_request_command("catalog-list"),
        active_catalog_response_owned_by_app(old_deployment, "100.64.0.9"),
    );
    responses.insert(
        image_inspect_command("docker", "docker.io/example/web:latest"),
        success(""),
    );
    responses.insert(mktemp_command(), cutover_generation_path("def456"));

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config_with_build(dir.path(), harness.addr, &key_path, "docker");

    let output = run_jiji_deploy(&config_path, &[]);
    assert!(
        output.status.success(),
        "expected success, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let received = harness.received.lock().unwrap().clone();
    let apply_index = received
        .iter()
        .position(|c| c.contains("# jiji-request:image-retention-apply"))
        .expect("an image-retention spec should have been pushed after deploy");
    let run_index = received
        .iter()
        .position(|c| c.contains("docker run --name demo-web-"))
        .expect("candidate should have been created");
    assert!(
        run_index < apply_index,
        "image-retention spec application must happen after the candidate is created/activated: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn deploy_does_not_push_an_image_retention_spec_for_a_static_image_service() {
    let (dir, key_path, client_key) = setup_test_dir();
    let generation = plan_generation(SocketAddr::from(([127, 0, 0, 1], 0)), "docker");

    let old_deployment = "olddeployment1234567890";
    let mut responses = HashMap::new();
    responses.insert(generation_path(), success(&format!("{generation}\n")));
    responses.insert(
        service_runtime_generation_path(),
        current_service_runtime_generation("docker"),
    );
    responses.insert(
        agent_request_command("catalog-list"),
        active_catalog_response(old_deployment, "100.64.0.9"),
    );
    responses.insert(
        image_inspect_command("docker", "docker.io/example/web:latest"),
        success(""),
    );
    responses.insert(mktemp_command(), cutover_generation_path("def456"));

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    // `write_config`, not `write_config_with_build`: "web" has no `build:` configured here.
    let config_path = write_config(dir.path(), harness.addr, &key_path, "docker");

    let output = run_jiji_deploy(&config_path, &[]);
    assert!(
        output.status.success(),
        "expected success, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let received = harness.received.lock().unwrap().clone();
    assert!(
        !received
            .iter()
            .any(|c| c.contains("# jiji-request:image-retention-apply")),
        "a static `image:` service must never get an image-retention spec: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn deploy_reports_a_partial_failure_when_cron_installation_fails_without_undeploying() {
    let (dir, key_path, client_key) = setup_test_dir();
    let generation = plan_generation(SocketAddr::from(([127, 0, 0, 1], 0)), "docker");

    let old_deployment = "olddeployment1234567890";
    let mut responses = HashMap::new();
    responses.insert(generation_path(), success(&format!("{generation}\n")));
    responses.insert(
        service_runtime_generation_path(),
        current_service_runtime_generation("docker"),
    );
    responses.insert(
        agent_request_command("catalog-list"),
        active_catalog_response_owned_by_app(old_deployment, "100.64.0.9"),
    );
    responses.insert(
        image_inspect_command("docker", "docker.io/example/web:latest"),
        success(""),
    );
    responses.insert(mktemp_command(), cutover_generation_path("def456"));
    responses.insert("pwd".to_string(), success("/root"));
    responses.insert(agent_request_command("cron-spec-apply"), failure());

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config_with_cron(dir.path(), harness.addr, &key_path, "docker");

    let output = run_jiji_deploy(&config_path, &[]);
    assert!(
        !output.status.success(),
        "a cron installation failure must be reported, not silently swallowed"
    );

    let received = harness.received.lock().unwrap().clone();
    assert!(
        received
            .iter()
            .any(|c| c.contains("docker run --name demo-web-")),
        "the service container must still have been deployed: {received:?}"
    );
    assert!(
        !received.iter().any(|c| c.contains("rm -f demo-web-")
            && !c.contains("olddeploymen")),
        "a cron-only failure must never remove the healthy candidate that was just deployed: {received:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Run `jiji deploy` again"),
        "the error must tell the operator to redeploy: {stderr}"
    );
}

/// A `type=cron_specs` response reporting one spec still installed for `web`'s `orphaned` cron --
/// used to simulate a cron entry that was renamed or deleted from `crons:` (or a service that
/// dropped `crons:` entirely) while a previous installation is still sitting on the agent.
fn cron_specs_response_with_one_orphan() -> CannedResponse {
    success(
        r#"{"Ok":{"type":"cron_specs","specs":[{"project":"demo","service":"web","cron_name":"orphaned","revision":1,"canonical_hash":"abc123","owner_node_id":"app","owner_epoch":1,"server":"app","source_deployment_id":"olddeployment1234567890","source_replica_id":"web-c1fe97ed0787","image":"docker.io/example/web:latest","schedule":"*/5 * * * *","timezone":"UTC","timeout_seconds":3600,"overlap":"forbid","missed_runs":"skip","command":["echo","hi"],"env_file_path":"/root/.jiji/demo/env/web-app.env","mount_args":[],"resource_args":[],"bridge_network":"jiji-demo","dns_address":"100.64.0.5"}]}}"#,
    )
}

/// Regression test: a cron spec that disappeared from configuration (renamed, deleted, or the
/// whole `crons:` block removed) must be swept up on the next deploy, not left running forever.
/// `web` here has no `crons:` at all -- the sweep must still run and find/remove the orphan
/// reported by the canned `cron-spec-list` response above, even though there is nothing to
/// install.
#[tokio::test(flavor = "multi_thread")]
async fn deploy_removes_a_cron_spec_that_disappeared_from_configuration() {
    let (dir, key_path, client_key) = setup_test_dir();
    let generation = plan_generation(SocketAddr::from(([127, 0, 0, 1], 0)), "docker");

    let mut responses = HashMap::new();
    responses.insert(generation_path(), success(&format!("{generation}\n")));
    responses.insert(
        service_runtime_generation_path(),
        current_service_runtime_generation("docker"),
    );
    responses.insert(
        image_inspect_command("docker", "docker.io/example/web:latest"),
        success(""),
    );
    responses.insert(mktemp_command(), cutover_generation_path("abc123"));
    responses.insert(
        agent_request_command("cron-spec-list"),
        cron_specs_response_with_one_orphan(),
    );
    responses.insert(
        agent_request_command("cron-spec-remove"),
        success(r#"{"Ok":{"type":"cron_spec_removed","removed":true}}"#),
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path, "docker");

    let output = run_jiji_deploy(&config_path, &[]);
    assert!(
        output.status.success(),
        "expected success, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let received = harness.received.lock().unwrap().clone();
    assert!(
        received
            .iter()
            .any(|c| c.contains("# jiji-request:cron-spec-remove")),
        "a cron spec no longer present in configuration must be removed from its agent: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn health_check_failure_removes_only_the_candidate_and_keeps_old_container_serving() {
    let (dir, key_path, client_key) = setup_test_dir();
    let generation = plan_generation(SocketAddr::from(([127, 0, 0, 1], 0)), "docker");

    let mut responses = HashMap::new();
    responses.insert(generation_path(), success(&format!("{generation}\n")));
    responses.insert(
        service_runtime_generation_path(),
        current_service_runtime_generation("docker"),
    );
    responses.insert(
        image_inspect_command("docker", "docker.io/example/web:latest"),
        success(""),
    );
    responses.insert("PREFIX:docker inspect demo-web-".to_string(), failure());

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path, "docker");

    let output = run_jiji_deploy(&config_path, &[]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "expected non-zero exit");
    assert!(
        stderr.contains("health") || stderr.contains("running"),
        "stderr: {stderr}"
    );

    let received = harness.received.lock().unwrap().clone();
    assert!(
        received
            .iter()
            .any(|c| c.contains("# jiji-request:release-address")),
        "the unhealthy candidate lease should be released: {received:?}"
    );
}

/// Base canned-response set for a successful `config_yaml_with_healthcheck` deploy, minus the
/// health-check command's own responses (each test registers those itself).
fn base_healthcheck_deploy_responses() -> HashMap<String, CannedResponse> {
    let generation = plan_generation(SocketAddr::from(([127, 0, 0, 1], 0)), "docker");
    let mut responses = HashMap::new();
    responses.insert(generation_path(), success(&format!("{generation}\n")));
    responses.insert(
        service_runtime_generation_path(),
        current_service_runtime_generation("docker"),
    );
    responses.insert(active_slots_path(), success(""));
    responses.insert(
        image_inspect_command("docker", "docker.io/example/web:latest"),
        success(""),
    );
    responses.insert(mktemp_command(), cutover_generation_path("hc001"));
    responses
}

#[tokio::test(flavor = "multi_thread")]
async fn health_check_progress_captures_stdout_from_a_failed_attempt() {
    let (dir, key_path, client_key) = setup_test_dir();
    let command = healthcheck_command();

    let mut responses = base_healthcheck_deploy_responses();
    responses.insert(
        format!("{command}#1"),
        CannedResponse {
            success: false,
            stdout: "still starting up".to_string(),
            stderr: String::new(),
        },
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config_with_healthcheck(
        dir.path(),
        harness.addr,
        &key_path,
        "docker",
        "1s",
        "5s",
        &[],
    );

    let output = run_jiji_deploy(&config_path, &["--skip-proxy"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("health check: still starting up"),
        "stdout: {stdout}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn health_check_progress_prefers_stderr_over_stdout() {
    let (dir, key_path, client_key) = setup_test_dir();
    let command = healthcheck_command();

    let mut responses = base_healthcheck_deploy_responses();
    responses.insert(
        format!("{command}#1"),
        CannedResponse {
            success: false,
            stdout: "stdout noise".to_string(),
            stderr: "connection refused".to_string(),
        },
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config_with_healthcheck(
        dir.path(),
        harness.addr,
        &key_path,
        "docker",
        "1s",
        "5s",
        &[],
    );

    let output = run_jiji_deploy(&config_path, &["--skip-proxy"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("health check: connection refused"),
        "stdout: {stdout}"
    );
    assert!(!stdout.contains("stdout noise"), "stdout: {stdout}");
}

#[tokio::test(flavor = "multi_thread")]
async fn health_check_progress_dedups_identical_attempts_but_reports_a_genuine_change() {
    let (dir, key_path, client_key) = setup_test_dir();
    let command = healthcheck_command();

    let mut responses = base_healthcheck_deploy_responses();
    for occurrence in [1, 2] {
        responses.insert(
            format!("{command}#{occurrence}"),
            CannedResponse {
                success: false,
                stdout: String::new(),
                stderr: "message A".to_string(),
            },
        );
    }
    responses.insert(
        format!("{command}#3"),
        CannedResponse {
            success: false,
            stdout: String::new(),
            stderr: "message B".to_string(),
        },
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config_with_healthcheck(
        dir.path(),
        harness.addr,
        &key_path,
        "docker",
        "1s",
        "10s",
        &[],
    );

    let output = run_jiji_deploy(&config_path, &["--skip-proxy"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.matches("health check: message A").count(),
        1,
        "an identical repeat should be deduped, stdout: {stdout}"
    );
    assert_eq!(
        stdout.matches("health check: message B").count(),
        1,
        "a genuinely changed attempt should still be reported, stdout: {stdout}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn health_check_timeout_reports_the_failure_text_and_the_log_tail() {
    let (dir, key_path, client_key) = setup_test_dir();
    let command = healthcheck_command();

    let mut responses = base_healthcheck_deploy_responses();
    responses.insert(
        command,
        CannedResponse {
            success: false,
            stdout: String::new(),
            stderr: "backend unreachable".to_string(),
        },
    );
    responses.insert(
        "PREFIX:docker logs --tail 50 demo-web-".to_string(),
        success("panic: crash on boot\n"),
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config_with_healthcheck(
        dir.path(),
        harness.addr,
        &key_path,
        "docker",
        "1s",
        "2s",
        &[],
    );

    let output = run_jiji_deploy(&config_path, &["--skip-proxy"]);
    assert!(!output.status.success(), "expected non-zero exit");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("backend unreachable"), "stderr: {stderr}");
    assert!(stderr.contains("panic: crash on boot"), "stderr: {stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn health_check_progress_never_prints_a_secret_value() {
    let (dir, key_path, client_key) = setup_test_dir();
    let command = healthcheck_command();

    let mut responses = base_healthcheck_deploy_responses();
    responses.insert(
        format!("{command}#1"),
        CannedResponse {
            success: false,
            stdout: "auth failed with token correct-horse-battery-staple".to_string(),
            stderr: String::new(),
        },
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config_with_healthcheck(
        dir.path(),
        harness.addr,
        &key_path,
        "docker",
        "1s",
        "5s",
        &["SECRET_TOKEN"],
    );

    let mut jiji_command = Command::new(env!("CARGO_BIN_EXE_jiji"));
    jiji_command
        .arg("deploy")
        .arg("-c")
        .arg(&config_path)
        .arg("--yes")
        .arg("--skip-proxy")
        .arg("--host-env")
        .env("SECRET_TOKEN", "correct-horse-battery-staple");
    let output = jiji_command.output().expect("run jiji deploy");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("health check: auth failed with token <redacted>"),
        "stdout: {stdout}"
    );
    assert!(
        !stdout.contains("correct-horse-battery-staple"),
        "stdout: {stdout}"
    );
    assert!(
        !stderr.contains("correct-horse-battery-staple"),
        "stderr: {stderr}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn podman_first_deployment_uses_podman_commands_only() {
    let (dir, key_path, client_key) = setup_test_dir();
    let generation = plan_generation(SocketAddr::from(([127, 0, 0, 1], 0)), "podman");

    let candidate_name = "demo-web-a";
    let mut responses = HashMap::new();
    responses.insert(generation_path(), success(&format!("{generation}\n")));
    responses.insert(
        service_runtime_generation_path(),
        current_service_runtime_generation("podman"),
    );
    responses.insert(active_slots_path(), success(""));
    responses.insert(inspect_status_command("podman", candidate_name), failure());
    responses.insert(
        image_inspect_command("podman", "docker.io/example/web:latest"),
        success(""),
    );
    responses.insert(
        readiness_health_command("podman", candidate_name),
        success(""),
    );
    responses.insert(mktemp_command(), cutover_generation_path("ghi789"));

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path, "podman");

    let output = run_jiji_deploy(&config_path, &[]);
    assert!(
        output.status.success(),
        "expected success, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let received = harness.received.lock().unwrap().clone();
    assert!(received.iter().any(|c| c.starts_with("podman run")));
    assert!(!received.iter().any(|c| c.starts_with("docker")));
}

async fn run_local_registry_deploy(
    pull_succeeds: bool,
) -> (
    std::process::Output,
    u16,
    Vec<(String, u32)>,
    Vec<(String, u32)>,
    Vec<String>,
) {
    let (dir, key_path, client_key) = setup_test_dir();
    let registry_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind registry");
    let registry_port = registry_listener
        .local_addr()
        .expect("registry address")
        .port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = registry_listener.accept().await {
            tokio::spawn(async move {
                let mut request = [0_u8; 256];
                let _ = stream.read(&mut request).await;
                let _ = stream
                    .write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 2\r\n\r\n{}")
                    .await;
            });
        }
    });

    let fake_bin = dir.path().join("bin");
    std::fs::create_dir(&fake_bin).expect("create fake bin");
    let docker = fake_bin.join("docker");
    std::fs::write(
        &docker,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then exit 0; fi\nif [ \"$1\" = \"container\" ] && [ \"$2\" = \"inspect\" ]; then printf 'true|registry|{registry_port}|true\\n'; exit 0; fi\nexit 0\n"
        ),
    )
    .expect("write fake docker");
    let mut permissions = std::fs::metadata(&docker)
        .expect("docker metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&docker, permissions).expect("make fake docker executable");

    let config = format!(
        r#"
project: demo
builder:
  engine: docker
  registry:
    type: local
    port: {registry_port}
servers:
  app:
    host: {host}
    port: {ssh_port}
    keys: [{key_path}]
services:
  web:
    build: .
    servers: [app]
ssh:
  user: tester
  keys_only: true
"#,
        host = "127.0.0.1",
        ssh_port = 0,
        key_path = key_path.display(),
    );
    let generation_config: Config = serde_yaml::from_str(&config).expect("parse generation config");
    let generation = NetworkPlanner::new()
        .plan(&generation_config)
        .expect("network plan")
        .mesh_generation;

    let mut responses = HashMap::new();
    responses.insert(generation_path(), success(&format!("{generation}\n")));
    responses.insert(
        service_runtime_generation_path(),
        current_service_runtime_generation("docker"),
    );
    responses.insert(active_slots_path(), success(""));
    responses.insert(inspect_status_command("docker", "demo-web-a"), failure());
    responses.insert(
        readiness_health_command("docker", "demo-web-a"),
        success(""),
    );
    responses.insert(mktemp_command(), cutover_generation_path("local123"));
    if !pull_succeeds {
        responses.insert(
            format!("docker pull localhost:{registry_port}/demo-web:v1"),
            failure(),
        );
    }
    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;

    let config = config.replace("port: 0", &format!("port: {}", harness.addr.port()));
    let config_path = dir.path().join("deploy-local.yml");
    std::fs::write(&config_path, config).expect("write local registry config");

    let existing_path = std::env::var_os("PATH").unwrap_or_default();
    let mut command = Command::new(env!("CARGO_BIN_EXE_jiji"));
    let output = command
        .arg("deploy")
        .arg("-c")
        .arg(&config_path)
        .arg("--yes")
        .arg("--build")
        .arg("--version")
        .arg("v1")
        .env(
            "PATH",
            std::env::join_paths(
                std::iter::once(fake_bin.as_path()).chain(
                    std::env::split_paths(&existing_path)
                        .collect::<Vec<_>>()
                        .iter()
                        .map(std::path::PathBuf::as_path),
                ),
            )
            .expect("join PATH"),
        )
        .output()
        .expect("run local registry deploy");

    let forwards = harness
        .forwards
        .lock()
        .expect("forwards mutex poisoned")
        .clone();
    let cancelled = harness
        .cancelled_forwards
        .lock()
        .expect("cancelled forwards mutex poisoned")
        .clone();
    let received = harness
        .received
        .lock()
        .expect("received mutex poisoned")
        .clone();
    (output, registry_port, forwards, cancelled, received)
}

#[tokio::test(flavor = "multi_thread")]
async fn local_registry_build_opens_a_loopback_reverse_tunnel_before_deploy() {
    let (output, registry_port, forwards, cancelled, received) =
        run_local_registry_deploy(true).await;
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        forwards,
        vec![("127.0.0.1".to_string(), u32::from(registry_port))]
    );
    assert_eq!(cancelled, forwards);
    assert!(received
        .iter()
        .any(|command| { command.contains(&format!("localhost:{registry_port}/demo-web:v1")) }));
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_pull_after_tunnel_setup_cancels_the_forward_and_stops_deploy() {
    let (output, registry_port, forwards, cancelled, received) =
        run_local_registry_deploy(false).await;
    assert!(!output.status.success());
    assert_eq!(
        forwards,
        vec![("127.0.0.1".to_string(), u32::from(registry_port))]
    );
    assert_eq!(cancelled, forwards);
    assert!(!received
        .iter()
        .any(|command| command.contains("run --name")));
}

#[tokio::test(flavor = "multi_thread")]
async fn deploy_bails_when_deployment_lock_is_held() {
    let (dir, key_path, client_key) = setup_test_dir();
    // This config deploys the single, deterministically-placed replica of service "web" on
    // server "app" (see `default_response`'s desired-read/desired-commit canned records), so
    // `jiji deploy` acquires exactly one `LogicalReplica` lock at this path.
    let lock_path =
        "cat .jiji/demo/locks/replica/web-c1fe97ed0787.lock/info.json 2>/dev/null || true";
    let mut responses = HashMap::new();
    responses.insert(atomic_lock_command(), success("JIJI_LOCK_HELD\n"));
    responses.insert(
        lock_path.to_string(),
        success(
            r#"{"message":"Deploying v1.2.3","acquired_at":1000000000,"acquired_by":"alice","pid":123}"#,
        ),
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path, "docker");

    let output = run_jiji_deploy(&config_path, &["--lock-timeout", "0"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success());
    assert!(
        stderr.contains("Could not acquire every lock this operation needs"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("Deploying v1.2.3"), "stderr: {stderr}");
    assert!(stderr.contains("alice"), "stderr: {stderr}");

    let received = harness.received.lock().unwrap().clone();
    assert!(
        !received.iter().any(|c| c.contains("run --name")),
        "no container should be touched while the lock is held: {received:?}"
    );
}

fn atomic_lock_command() -> String {
    "PREFIX:set -eu\nmkdir -p .jiji/demo/locks/replica\nmkdir .jiji/demo/locks/replica/web-c1fe97ed0787.lock.".to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn deploy_gives_an_actionable_hint_when_no_agent_is_installed() {
    let (dir, key_path, client_key) = setup_test_dir();
    // Simulates a host that has never run `jiji server setup` (or predates jiji-agent
    // entirely): the remote `jiji-agent` binary itself doesn't exist, so *every* agent
    // request fails at the SSH-command level (a shell "No such file or directory"), not just
    // the health check -- matching what a real host with no agent installed actually does.
    let mut responses = HashMap::new();
    responses.insert(
        "PREFIX:/etc/jiji/agent/demo-354b6884/bin/jiji-agent".to_string(),
        CannedResponse {
            success: false,
            stdout: String::new(),
            stderr: "bash: line 1: /etc/jiji/agent/demo-354b6884/bin/jiji-agent: \
                      No such file or directory"
                .to_string(),
        },
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path, "docker");

    let output = run_jiji_deploy(&config_path, &[]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success());
    assert!(
        stderr.contains("Could not reach jiji-agent") && stderr.contains("jiji server setup"),
        "stderr should give an actionable hint instead of the raw remote-command error: {stderr}"
    );
    assert!(
        !stderr.contains("No such file or directory"),
        "the raw shell error should never reach the user: {stderr}"
    );

    let received = harness.received.lock().unwrap().clone();
    assert!(
        !received.iter().any(|c| c.contains("run --name")),
        "no container should be touched when the agent can't be reached: {received:?}"
    );
}

/// Identical to `config_yaml` except the "web" service declares a raw TCP proxy target
/// (`listen_port`) instead of no `proxy:` block at all.
fn config_yaml_with_tcp_route(
    addr: SocketAddr,
    key_path: &std::path::Path,
    engine: &str,
) -> String {
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
    proxy:
      port: 5432
      listen_port: 5432
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

fn write_config_with_tcp_route(
    dir: &std::path::Path,
    addr: SocketAddr,
    key_path: &std::path::Path,
    engine: &str,
) -> std::path::PathBuf {
    let config_path = dir.join("deploy.yml");
    std::fs::write(
        &config_path,
        config_yaml_with_tcp_route(addr, key_path, engine),
    )
    .expect("write test deploy.yml");
    config_path
}

/// The "app" server's own `.jiji` resolver address, exactly as `deploy_transaction.rs` computes
/// it (`SocketAddr::new(ctx.server.dns_address.into(), 53)`) -- needed to build the exact
/// `jiji-proxy tcp-route apply --dns-server=...` command string this test asserts on.
fn dns_server_for_app(addr: SocketAddr, engine: &str) -> SocketAddr {
    let yaml = config_yaml_with_tcp_route(addr, std::path::Path::new("/dev/null"), engine);
    let config: Config = serde_yaml::from_str(&yaml).expect("parse test config");
    let plan = NetworkPlanner::new()
        .plan(&config)
        .expect("build test plan");
    SocketAddr::new(plan.servers["app"].dns_address.into(), 53)
}

#[tokio::test(flavor = "multi_thread")]
async fn first_deployment_with_a_tcp_route_applies_and_verifies_it() {
    let (dir, key_path, client_key) = setup_test_dir();
    let generation = plan_generation(SocketAddr::from(([127, 0, 0, 1], 0)), "docker");
    let dns_server = dns_server_for_app(SocketAddr::from(([127, 0, 0, 1], 0)), "docker");

    let mut responses = HashMap::new();
    responses.insert(generation_path(), success(&format!("{generation}\n")));
    responses.insert(
        service_runtime_generation_path(),
        current_service_runtime_generation("docker"),
    );
    responses.insert(
        image_inspect_command("docker", "docker.io/example/web:latest"),
        success(""),
    );
    responses.insert(mktemp_command(), cutover_generation_path("tcp123"));
    // Adding a proxy: block makes "app" an ingress host, so `jiji deploy`'s "Verifying Proxy:"
    // phase now runs `ensure_proxy` -- report jiji-proxy as already running with a matching
    // fingerprint so it skips the (slow, 30-retry) recreate/wait_until_running path entirely.
    responses.insert(
        "docker inspect jiji-proxy --format '{{.State.Status}} {{index .Config.Labels \"jiji.proxy-config\"}} {{.Config.Image}}'".to_string(),
        success(&format!("running v1-docker {}\n", jiji_network::image())),
    );
    responses.insert(
        format!(
            "docker exec jiji-proxy jiji-proxy tcp-route apply --listen-port=5432 --dns-server={dns_server} --name=demo-web.jiji --port=5432"
        ),
        success(""),
    );
    responses.insert(
        "docker exec jiji-proxy jiji-proxy tcp-route status --listen-port=5432".to_string(),
        success(
            r#"{"route_exists":true,"backends":[{"address":"100.64.0.10:5432","healthy":true}]}"#,
        ),
    );
    // `reconcile_catalog_routes` (runs once after every selected endpoint finishes) re-applies
    // and then verifies every proxy-enabled service's routes via `tcp-route list`.
    responses.insert(
        "docker exec jiji-proxy jiji-proxy tcp-route list".to_string(),
        success(r#"[{"listen_port":5432}]"#),
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config_with_tcp_route(dir.path(), harness.addr, &key_path, "docker");

    let output = run_jiji_deploy(&config_path, &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "expected success, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("demo:web:web-") && stdout.contains(": deployed"),
        "stdout: {stdout}"
    );

    let received = harness.received.lock().unwrap().clone();
    assert!(
        received
            .iter()
            .any(|c| c.contains("jiji-proxy tcp-route apply --listen-port=5432")),
        "the tcp route should have been applied: {received:?}"
    );
    assert!(
        received
            .iter()
            .any(|c| c.contains("jiji-proxy tcp-route status --listen-port=5432")),
        "the tcp route should have been verified: {received:?}"
    );
    // No HTTP route command should ever be rendered for a listen_port-only target.
    assert!(
        !received
            .iter()
            .any(|c| c.contains("jiji-proxy route apply")),
        "a TCP-only target must not also produce an HTTP route apply: {received:?}"
    );
}
