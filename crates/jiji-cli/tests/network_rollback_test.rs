//! Integration test for the rollback-cleanup gap found during live Podman testing: a first-install
//! `jiji network setup` that fails *during* activation (inside `systemctl restart
//! jiji-network-restore-{slug}.service`, which runs the freshly staged `restore.sh`) can leave
//! an engine-level bridge network behind that `rollback_host`'s original symlink/unit-only
//! rollback command had no way to know about,
//! since it only reverts jiji's own compiled state, not whatever an external script partially did.
//! `rollback_host` (`commands/network/setup.rs`) now additionally calls
//! `network_teardown::remove_bridge_and_engine_network` whenever there was no previous generation
//! to fall back to (`state.network.is_none()`, i.e. a first install).
//!
//! Uses the same in-process russh `TestServer`/`CannedResponse`/occurrence-keyed harness pattern as
//! `deploy_test.rs`/`multi_project_network_test.rs`, with one addition: a substring-based failure
//! override for the activation command specifically (its exact text embeds a content hash that
//! can't be predicted ahead of time from outside the module), identified by `sysctl --system`,
//! which appears nowhere else in `jiji network setup`'s command set (confirmed: it's not part of
//! `render_rollback_command`, so this override can't accidentally fail the rollback itself).

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

fn failure(stderr: &str) -> CannedResponse {
    CannedResponse {
        success: false,
        stdout: String::new(),
        stderr: stderr.to_string(),
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
        // lines of output (see `network::setup::parse_installed_generation`); reporting "-\n-\n"
        // (no previous generation on either symlink) is what puts this test in the first-install
        // scenario the fix targets (mirrors `server_setup_test.rs`/`multi_project_network_test.rs`'s
        // identical special case).
        let response = if command.contains("if test -L ") && command.contains("/current") {
            success("-\n-\n")
        } else if command.contains("sysctl --system") && command.contains("rollback-demo") {
            // Marks the activate_host command specifically (present nowhere else in the setup
            // command set, including the rollback command it triggers) so it can be failed without
            // knowing its exact text ahead of time -- it embeds a content hash of the generation.
            failure("simulated failure inside restore.sh, e.g. a bad nftables rule")
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

fn plan_for(project: &str, addr: SocketAddr) -> jiji_network::NetworkPlan {
    let yaml = config_yaml(project, addr, std::path::Path::new("/dev/null"));
    let config: Config = serde_yaml::from_str(&yaml).expect("parse test config");
    NetworkPlanner::new()
        .plan(&config)
        .expect("build test plan")
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

#[tokio::test(flavor = "multi_thread")]
async fn first_install_activation_failure_removes_the_partially_created_bridge() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let (key_path, client_key) = setup_key(dir.path(), "id");

    let project = "rollback-demo";
    let plan = plan_for(project, SocketAddr::from(([127, 0, 0, 1], 0)));
    let server = &plan.servers["app"];
    let bridge_name = server.bridge_name.clone();
    let slug = jiji_network::systemd_unit_slug(project);

    let mut responses = HashMap::new();
    responses.insert("id -u".to_string(), success("0\n"));
    let public_key_path = format!("/etc/jiji/network/{slug}/public.key");
    responses.insert(
        format!("test -s {public_key_path} && cat {public_key_path}"),
        success("rollback-demo-public-key\n"),
    );

    let harness = spawn_test_server(vec![client_key.public_key().clone()], responses).await;
    let config_path = dir.path().join("deploy.yml");
    std::fs::write(&config_path, config_yaml(project, harness.addr, &key_path))
        .expect("write test config");

    let output = Command::new(env!("CARGO_BIN_EXE_jiji"))
        .arg("network")
        .arg("setup")
        .arg("-c")
        .arg(&config_path)
        .output()
        .expect("run jiji network setup");

    assert!(
        !output.status.success(),
        "network setup should fail when activation fails, stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let received = harness.received.lock().unwrap().clone();
    assert!(
        received.iter().any(|c| c.contains("sysctl --system")),
        "activation should have been attempted: {received:?}"
    );
    assert!(
        received
            .iter()
            .any(|c| c == &format!("docker network inspect {bridge_name} >/dev/null 2>&1")),
        "rollback should probe whether the partially-created bridge exists: {received:?}"
    );
    assert!(
        received
            .iter()
            .any(|c| c == &format!("docker network rm {bridge_name}")),
        "rollback should remove the partially-created bridge left behind by the failed \
         activation, not just jiji's own compiled-state symlinks: {received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn setup_migrates_an_existing_bridge_and_reattaches_the_proxy() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let (key_path, client_key) = setup_key(dir.path(), "migration-id");

    let project = "migration-demo";
    let plan = plan_for(project, SocketAddr::from(([127, 0, 0, 1], 0)));
    let server = &plan.servers["app"];
    let bridge_name = server.bridge_name.clone();
    let slug = jiji_network::systemd_unit_slug(project);

    let probe = format!(
        "set -eu; if ! docker network inspect {bridge_name} >/dev/null 2>&1; then printf '%s\\n' MISSING; exit 0; fi; \
         subnet=$(docker network inspect {bridge_name} --format '{{{{(index .IPAM.Config 0).Subnet}}}}'); \
         gateway=$(docker network inspect {bridge_name} --format '{{{{(index .IPAM.Config 0).Gateway}}}}'); \
         printf '%s|%s\\n' \"$subnet\" \"$gateway\""
    );
    let list = format!("docker ps -a --filter network={bridge_name} --format '{{{{.Names}}}}'");
    let inspect_proxy = format!(
        "docker inspect kamal-proxy --format '{{{{(index .NetworkSettings.Networks \"{bridge_name}\").IPAddress}}}}'"
    );

    let mut responses = HashMap::new();
    responses.insert("id -u".to_string(), success("0\n"));
    responses.insert(probe, success("192.0.2.0/24|192.0.2.1\n"));
    responses.insert(list, success("kamal-proxy\n"));
    responses.insert(inspect_proxy, success("192.0.2.4\n"));
    let public_key_path = format!("/etc/jiji/network/{slug}/public.key");
    responses.insert(
        format!("test -s {public_key_path} && cat {public_key_path}"),
        success("migration-demo-public-key\n"),
    );

    let harness = spawn_test_server(vec![client_key.public_key().clone()], responses).await;
    let config_path = dir.path().join("migration.yml");
    std::fs::write(&config_path, config_yaml(project, harness.addr, &key_path))
        .expect("write test config");

    let output = Command::new(env!("CARGO_BIN_EXE_jiji"))
        .arg("network")
        .arg("setup")
        .arg("-c")
        .arg(&config_path)
        .output()
        .expect("run jiji network setup");

    assert!(
        output.status.success(),
        "network migration should succeed, stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let received = harness.received.lock().unwrap().clone();
    assert!(
        received.iter().any(|command| {
            command.contains(&format!(
                "docker network disconnect -f {bridge_name} kamal-proxy"
            )) && command.contains(&format!("docker network rm {bridge_name}"))
        }),
        "migration should detach the proxy and remove the old bridge: {received:?}"
    );
    assert!(
        received.iter().any(|command| {
            command
                == &format!(
                    "docker network connect --ip {} {bridge_name} kamal-proxy",
                    server.proxy_address
                )
        }),
        "migration should reconnect the proxy at its new planned address: {received:?}"
    );
}
