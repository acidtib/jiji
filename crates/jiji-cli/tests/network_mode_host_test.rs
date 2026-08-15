//! Integration tests for `network_mode: "host"`, mirroring `network_mode_service_test.rs`'s
//! in-process mock-SSH pattern. Unlike that harness, this one also captures each exec channel's
//! stdin (the JSON `jiji-agent request` body) so the catalog-commit address can be asserted on
//! directly, not just inferred from the rendered `docker run` command.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
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
        r#"{"Ok":{"type":"desired_state","record":{"project_id":"demo","recovery_epoch":1,"protocol_version":1,"schema_version":1,"service":"app","replica_override":1,"assignments":[{"replica_id":"app-test","ordinal":0,"owner_node_id":"app"}],"revision":1,"author_node_id":"app","author_epoch":1}}}"#
    } else if command.contains("# jiji-request:desired-read") {
        r#"{"Ok":{"type":"desired_state","record":null}}"#
    } else if command.contains("# jiji-request:catalog-commit") {
        r#"{"Ok":{"type":"catalog_committed","record":{"project_id":"demo","recovery_epoch":1,"protocol_version":1,"schema_version":2,"service":"app","replica_id":"app-test","owner_node_id":"app","owner_epoch":1,"revision":1,"deployment_id":"aabbccddeeff001122","address":"100.64.0.1","ports":[8080],"image":"docker.io/library/nginx:latest","state":"active","health":"healthy"}}}"#
    } else if command.contains("# jiji-request:release-address") {
        r#"{"Ok":{"type":"address_released","released":true}}"#
    } else if command.contains("# jiji-request:cron-spec-list") {
        r#"{"Ok":{"type":"cron_specs","specs":[]}}"#
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

#[derive(Clone)]
struct TestServer {
    authorized_key: PublicKey,
    responses: Arc<HashMap<String, CannedResponse>>,
    pending: Arc<Mutex<HashMap<ChannelId, String>>>,
    stdin: Arc<Mutex<HashMap<ChannelId, Vec<u8>>>>,
    received: Arc<Mutex<Vec<String>>>,
    received_payloads: Arc<Mutex<Vec<(String, String)>>>,
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
        let stdin_bytes = self
            .stdin
            .lock()
            .expect("stdin mutex poisoned")
            .remove(&channel)
            .unwrap_or_default();
        self.received_payloads
            .lock()
            .expect("received_payloads mutex poisoned")
            .push((
                command.clone(),
                String::from_utf8_lossy(&stdin_bytes).into_owned(),
            ));

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
    received_payloads: Arc<Mutex<Vec<(String, String)>>>,
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
    let received_payloads = Arc::new(Mutex::new(Vec::new()));
    let mut test_server = TestServer {
        authorized_key,
        responses: Arc::new(responses),
        pending: Arc::new(Mutex::new(HashMap::new())),
        stdin: Arc::new(Mutex::new(HashMap::new())),
        received: received.clone(),
        received_payloads: received_payloads.clone(),
    };

    tokio::spawn(async move {
        let _ = test_server.run_on_socket(config, &listener).await;
        drop(listener);
    });

    Harness {
        addr,
        received,
        received_payloads,
    }
}

/// A single `network_mode: host` service ("app") on a single server ("app"), listening on 8080.
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
  app:
    image: nginx:latest
    servers: [app]
    network_mode: host
    ports: ["8080"]
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
    plan(addr).mesh_generation
}

fn plan(addr: SocketAddr) -> jiji_network::NetworkPlan {
    let yaml = config_yaml(addr, std::path::Path::new("/dev/null"));
    let config: Config = serde_yaml::from_str(&yaml).expect("parse test config");
    NetworkPlanner::new()
        .plan(&config)
        .expect("build test plan")
}

fn management_address(addr: SocketAddr) -> Ipv4Addr {
    plan(addr).servers["app"].management_address
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

fn run_jiji_service_remove(
    config_path: &std::path::Path,
    extra_args: &[&str],
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_jiji"));
    command
        .arg("service")
        .arg("remove")
        .arg("-c")
        .arg(config_path)
        .arg("-y");
    for arg in extra_args {
        command.arg(arg);
    }
    command.output().expect("run jiji service remove")
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
        image_inspect_command("docker.io/library/nginx:latest"),
        success(""),
    );
    responses
}

#[tokio::test(flavor = "multi_thread")]
async fn host_mode_deploy_renders_network_host_without_ports_or_lease() {
    let (dir, key_path, client_key) = setup_test_dir();
    let generation = plan_generation(SocketAddr::from(([127, 0, 0, 1], 0)));
    let responses = base_responses(&generation);

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);
    let expected_address = management_address(harness.addr);

    let output = run_jiji_deploy(&config_path, &["-S", "app"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");

    let received = harness.received.lock().unwrap().clone();
    assert!(
        received
            .iter()
            .any(|c| c.contains("docker run --name demo-app-")
                && c.contains("--network host")
                && !c.contains("--ip")
                && !c.contains("-p 8080")),
        "app should run with --network host, no --ip, no -p: {received:?}"
    );
    assert!(
        !received
            .iter()
            .any(|c| c.contains("# jiji-request:allocate-address")),
        "a host-mode deploy must never lease an address: {received:?}"
    );

    let payloads = harness.received_payloads.lock().unwrap().clone();
    let active_commit = payloads.iter().find(|(command, stdin)| {
        command.contains("# jiji-request:catalog-commit") && stdin.contains("\"state\":\"active\"")
    });
    let (_, stdin) = active_commit.expect("expected an Active catalog-commit payload");
    assert!(
        stdin.contains(&format!("\"address\":\"{expected_address}\"")),
        "Active catalog record should carry the server's management_address {expected_address}: {stdin}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn host_mode_remove_issues_its_normal_no_op_release_call() {
    let (dir, key_path, client_key) = setup_test_dir();
    let generation = plan_generation(SocketAddr::from(([127, 0, 0, 1], 0)));
    let mut responses = base_responses(&generation);
    let expected_address = management_address(SocketAddr::from(([127, 0, 0, 1], 0)));
    responses.insert(
        "docker inspect demo-app-aabbccddeeff --format '{{.State.Status}}'".to_string(),
        success("running\n"),
    );
    responses.insert(
        format!(
            "/etc/jiji/agent/{}/bin/jiji-agent request --socket /etc/jiji/agent/{}/agent.sock # jiji-request:catalog-list",
            slug(),
            slug()
        ),
        success(&format!(
            r#"{{"Ok":{{"type":"catalog_list","records":[{{"project_id":"demo","recovery_epoch":1,"protocol_version":1,"schema_version":2,"service":"app","replica_id":"app-test","owner_node_id":"app","owner_epoch":1,"revision":1,"deployment_id":"aabbccddeeff001122","address":"{expected_address}","ports":[8080],"image":"docker.io/library/nginx:latest","state":"active","health":"healthy"}}]}}}}"#
        )),
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), harness.addr, &key_path);

    let output = run_jiji_service_remove(&config_path, &["-S", "app"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");

    let received = harness.received.lock().unwrap().clone();
    assert!(received.contains(&"docker stop demo-app-aabbccddeeff".to_string()));
    assert!(received.contains(&"docker rm -f demo-app-aabbccddeeff".to_string()));
    assert!(received
        .iter()
        .any(|command| command.contains("# jiji-request:catalog-commit")));
    assert!(
        received
            .iter()
            .any(|command| command.contains("# jiji-request:release-address")),
        "remove must still issue its normal (no-op) release call even though host mode never leased an address: {received:?}"
    );
}
