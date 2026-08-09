//! Integration tests for `jiji service remove`, run as a real subprocess against a real,
//! in-process SSH server (mirroring `service_restart_test.rs`'s minimal harness). Every test
//! passes `-y` to skip the interactive confirmation prompt, matching `server_teardown_test.rs`'s
//! convention -- a captured-pipe test subprocess has no controlling terminal to answer it.

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
        r#"{"Ok":{"type":"catalog_list","records":[{"project_id":"demo","recovery_epoch":1,"protocol_version":1,"schema_version":2,"service":"web","replica_id":"web-c1fe97ed0787","owner_node_id":"app","owner_epoch":1,"revision":2,"deployment_id":"abcdef1234567890","address":"100.64.0.9","ports":[3000],"image":"docker.io/example/web:latest","state":"active","health":"healthy"}]}}"#
    } else if command.contains("# jiji-request:catalog-commit") {
        r#"{"Ok":{"type":"catalog_committed","record":{"project_id":"demo","recovery_epoch":1,"protocol_version":1,"schema_version":2,"service":"web","replica_id":"web-c1fe97ed0787","owner_node_id":"app","owner_epoch":1,"revision":3,"deployment_id":"abcdef1234567890","address":"100.64.0.9","ports":[3000],"image":"docker.io/example/web:latest","state":"stopped","health":"unhealthy"}}}"#
    } else if command.contains("# jiji-request:release-address") {
        r#"{"Ok":{"type":"address_released","released":true}}"#
    } else if command.contains("# jiji-request:cron-spec-list") {
        r#"{"Ok":{"type":"cron_specs","specs":[]}}"#
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

/// A single service ("web", image "example/web:latest", proxy on port 3000) on a single
/// server ("app"), with a named volume so `--volumes` has something to remove.
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
    image: example/web:latest
    servers: [app]
    volumes: ["web_storage:/data"]
    proxy:
      port: 3000
      hosts: [example.com]
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

fn inspect_status_command(name: &str) -> String {
    format!("docker inspect {name} --format '{{{{.State.Status}}}}'")
}

#[tokio::test(flavor = "multi_thread")]
async fn remove_retires_catalog_deployment_and_removes_the_route() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert(
        inspect_status_command("demo-web-abcdef123456"),
        success("running\n"),
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), &config_yaml(harness.addr, &key_path));

    let output = run_jiji_service_remove(&config_path, &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        stdout.contains("container 'demo-web-abcdef123456': removed"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("catalog deployment 'abcdef1234567890': removed"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("proxy route 'example.com': removed"),
        "stdout: {stdout}"
    );
    assert!(!stdout.contains("VIP mapping"), "stdout: {stdout}");

    let received = harness.received.lock().unwrap().clone();
    assert!(received.contains(&"docker stop demo-web-abcdef123456".to_string()));
    assert!(received.contains(&"docker rm -f demo-web-abcdef123456".to_string()));
    assert!(received
        .iter()
        .any(|command| command.contains("# jiji-request:catalog-commit")));
    assert!(received
        .iter()
        .any(|command| command.contains("# jiji-request:release-address")));
    assert!(received.contains(
        &"docker exec jiji-proxy jiji-proxy route remove --host=example.com".to_string()
    ));
    assert!(
        !received.iter().any(|c| c.contains("volume rm")),
        "volumes must not be touched without --volumes: {received:?}"
    );
    assert!(
        received
            .iter()
            .any(|c| c.contains("cat >> .jiji/demo/audit.log")),
        "a successful remove should append an audit entry: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn remove_with_volumes_flag_removes_named_volumes() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert(
        inspect_status_command("demo-web-abcdef123456"),
        success("running\n"),
    );
    responses.insert(
        "docker volume inspect web-web_storage >/dev/null 2>&1".to_string(),
        success(""),
    );
    responses.insert(
        "docker ps -a --filter volume=web-web_storage --format '{{.Label \"jiji.project\"}}'"
            .to_string(),
        success("demo\n"),
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), &config_yaml(harness.addr, &key_path));

    let output = run_jiji_service_remove(&config_path, &["--volumes"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        stdout.contains("volume 'web-web_storage': removed"),
        "stdout: {stdout}"
    );

    let received = harness.received.lock().unwrap().clone();
    assert!(received.contains(&"docker volume rm web-web_storage".to_string()));
}

#[tokio::test(flavor = "multi_thread")]
async fn remove_without_yes_prompts_and_cancels_on_no_tty() {
    let (dir, key_path, client_key) = setup_test_dir();
    let responses = HashMap::new();
    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), &config_yaml(harness.addr, &key_path));

    let mut command = Command::new(env!("CARGO_BIN_EXE_jiji"));
    command
        .arg("service")
        .arg("remove")
        .arg("-c")
        .arg(&config_path);
    let output = command.output().expect("run jiji service remove");
    assert!(!output.status.success());
}

/// A concurrent operation already holding the owned replica's lock blocks removal -- the first
/// time `service remove` has ever taken a lock at all (see `crate::lock::LockScope::LogicalReplica`).
#[tokio::test(flavor = "multi_thread")]
async fn remove_is_blocked_while_a_concurrent_operation_holds_the_replica_lock() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert(
        "PREFIX:set -eu\nmkdir -p .jiji/demo/locks/replica\nmkdir .jiji/demo/locks/replica/web-c1fe97ed0787.lock.".to_string(),
        success("JIJI_LOCK_HELD\n"),
    );
    responses.insert(
        "cat .jiji/demo/locks/replica/web-c1fe97ed0787.lock/info.json 2>/dev/null || true"
            .to_string(),
        success(
            r#"{"message":"jiji deploy: web","acquired_at":1000000000,"acquired_by":"alice","pid":123}"#,
        ),
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), &config_yaml(harness.addr, &key_path));

    let output = run_jiji_service_remove(&config_path, &["--lock-timeout", "0"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("Could not acquire every lock this operation needs"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("alice"), "stderr: {stderr}");

    let received = harness.received.lock().unwrap().clone();
    assert!(
        !received.iter().any(|c| c.contains("docker stop")),
        "no container should be touched while the replica lock is held: {received:?}"
    );
}

/// Removing an endpoint with nothing owned in the catalog takes no *replica* lock -- there is no
/// replica to serialize against (a `HostGlobalProxy` lock is still taken separately here, since
/// this config's service has a `proxy:` route that removal unconditionally attempts to withdraw
/// regardless of catalog ownership).
#[tokio::test(flavor = "multi_thread")]
async fn remove_takes_no_replica_lock_when_nothing_is_owned() {
    let (dir, key_path, client_key) = setup_test_dir();
    let mut responses = HashMap::new();
    responses.insert(
        "/etc/jiji/agent/demo-354b6884/bin/jiji-agent request --socket \
         /etc/jiji/agent/demo-354b6884/agent.sock # jiji-request:catalog-list"
            .to_string(),
        success(r#"{"Ok":{"type":"catalog_list","records":[]}}"#),
    );

    let harness = spawn_test_server(client_key.public_key().clone(), responses).await;
    let config_path = write_config(dir.path(), &config_yaml(harness.addr, &key_path));

    let output = run_jiji_service_remove(&config_path, &[]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");

    let received = harness.received.lock().unwrap().clone();
    assert!(
        !received
            .iter()
            .any(|c| c.contains("mkdir .jiji/demo/locks/replica")),
        "removing something already absent must not acquire a replica lock: {received:?}"
    );
}
