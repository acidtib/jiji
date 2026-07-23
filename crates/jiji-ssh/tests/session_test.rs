//! Integration tests against a real, in-process SSH server (russh's own `server` module), so
//! connect/auth/exec are exercised at the protocol level instead of just checking struct wiring.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rand::rng;
use russh::keys::ssh_key::LineEnding;
use russh::keys::{Algorithm, PrivateKey, PublicKey};
use russh::server::{self, Auth, ChannelOpenHandle, Server as _, Session};
use russh::{Channel, ChannelId};
use tokio::net::TcpListener;

use jiji_ssh::{ConnectOptions, SshError, SshSession};

#[derive(Clone)]
struct TestServer {
    authorized_key: PublicKey,
    pending: Arc<Mutex<HashMap<ChannelId, String>>>,
    stdin: Arc<Mutex<HashMap<ChannelId, Vec<u8>>>>,
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

    // Deferring the response to EOF (rather than answering inside `exec_request`) sidesteps a
    // race: the client pipelines exec + data + eof without waiting for replies, so responding
    // immediately could close the channel before the client's own data/eof messages arrive.
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
        let stdin = self
            .stdin
            .lock()
            .expect("stdin mutex poisoned")
            .remove(&channel)
            .unwrap_or_default();

        match command.as_str() {
            // Never responds, to exercise the client's command_timeout.
            "hang" => return Ok(()),
            "fail" => {
                session.extended_data(channel, 1, "boom\n".to_string())?;
                session.exit_status_request(channel, 7)?;
            }
            _ => {
                let mut out = format!("ran: {command}\n");
                if !stdin.is_empty() {
                    out.push_str(&format!("stdin: {}\n", String::from_utf8_lossy(&stdin)));
                }
                session.data(channel, out)?;
                session.exit_status_request(channel, 0)?;
            }
        }

        session.eof(channel)?;
        session.close(channel)?;
        Ok(())
    }
}

/// Starts an in-process SSH server on a random loopback port, accepting only `authorized_key`.
async fn spawn_test_server(authorized_key: PublicKey) -> SocketAddr {
    let config = Arc::new(server::Config {
        auth_rejection_time: Duration::from_millis(50),
        keys: vec![PrivateKey::random(&mut rng(), Algorithm::Ed25519).expect("generate host key")],
        ..Default::default()
    });

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test listener");
    let addr = listener.local_addr().expect("read listener addr");

    let mut test_server = TestServer {
        authorized_key,
        pending: Arc::new(Mutex::new(HashMap::new())),
        stdin: Arc::new(Mutex::new(HashMap::new())),
    };

    tokio::spawn(async move {
        let _ = test_server.run_on_socket(config, &listener).await;
        // Keep the listener alive for the lifetime of the spawned task.
        drop(listener);
    });

    addr
}

fn generate_client_key() -> PrivateKey {
    PrivateKey::random(&mut rng(), Algorithm::Ed25519).expect("generate client key")
}

fn write_key_file(dir: &tempfile::TempDir, key: &PrivateKey) -> std::path::PathBuf {
    let path = dir.path().join("id_ed25519");
    let pem = key
        .to_openssh(LineEnding::LF)
        .expect("encode key as openssh");
    std::fs::write(&path, pem.as_bytes()).expect("write key file");
    path
}

fn base_options(addr: SocketAddr) -> ConnectOptions {
    let mut options = ConnectOptions::new(addr.ip().to_string(), "tester");
    options.port = addr.port();
    options.connect_timeout = Duration::from_secs(5);
    options.command_timeout = Duration::from_secs(5);
    options
}

#[tokio::test]
async fn connects_and_executes_with_a_key_file() {
    let client_key = generate_client_key();
    let addr = spawn_test_server(client_key.public_key().clone()).await;
    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = write_key_file(&dir, &client_key);

    let mut options = base_options(addr);
    options.keys = vec![key_path];
    options.keys_only = true;

    let session = SshSession::connect(&options).await.expect("connect");
    assert!(session.is_connected());
    assert_eq!(session.host(), addr.ip().to_string());

    let result = session.execute("echo hello").await.expect("execute");
    assert!(result.success);
    assert_eq!(result.code, Some(0));
    assert!(result.stdout.contains("ran: echo hello"));

    session.close().await;
}

#[tokio::test]
async fn connects_and_executes_with_inline_key_data() {
    let client_key = generate_client_key();
    let addr = spawn_test_server(client_key.public_key().clone()).await;
    let pem = client_key
        .to_openssh(LineEnding::LF)
        .expect("encode key as openssh")
        .to_string();

    let mut options = base_options(addr);
    options.key_data = vec![pem];
    options.keys_only = true;

    let session = SshSession::connect(&options).await.expect("connect");
    let result = session.execute("whoami").await.expect("execute");
    assert!(result.success);
}

#[tokio::test]
async fn rejects_a_key_the_server_does_not_recognize() {
    let authorized_key = generate_client_key();
    let addr = spawn_test_server(authorized_key.public_key().clone()).await;

    let untrusted_key = generate_client_key();
    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = write_key_file(&dir, &untrusted_key);

    let mut options = base_options(addr);
    options.keys = vec![key_path];
    options.keys_only = true;

    // `SshSession` doesn't implement `Debug` (it wraps a russh connection handle), so this can't
    // use `Result::expect_err` the way the other tests do.
    let err = match SshSession::connect(&options).await {
        Ok(_) => panic!("expected authentication to fail"),
        Err(err) => err,
    };
    assert!(
        matches!(err, SshError::Auth { .. }),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn execute_with_input_sends_stdin_to_the_remote_command() {
    let client_key = generate_client_key();
    let addr = spawn_test_server(client_key.public_key().clone()).await;
    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = write_key_file(&dir, &client_key);

    let mut options = base_options(addr);
    options.keys = vec![key_path];
    options.keys_only = true;

    let session = SshSession::connect(&options).await.expect("connect");
    let result = session
        .execute_with_input("cat", b"piped data")
        .await
        .expect("execute with input");

    assert!(result.success);
    assert!(result.stdout.contains("stdin: piped data"));
}

#[tokio::test]
async fn a_failing_remote_command_is_reported_as_unsuccessful() {
    let client_key = generate_client_key();
    let addr = spawn_test_server(client_key.public_key().clone()).await;
    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = write_key_file(&dir, &client_key);

    let mut options = base_options(addr);
    options.keys = vec![key_path];
    options.keys_only = true;

    let session = SshSession::connect(&options).await.expect("connect");
    let result = session.execute("fail").await.expect("execute");

    assert!(!result.success);
    assert_eq!(result.code, Some(7));
    assert!(result.stderr.contains("boom"));
}

#[tokio::test]
async fn a_command_that_never_responds_times_out() {
    let client_key = generate_client_key();
    let addr = spawn_test_server(client_key.public_key().clone()).await;
    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = write_key_file(&dir, &client_key);

    let mut options = base_options(addr);
    options.keys = vec![key_path];
    options.keys_only = true;
    options.command_timeout = Duration::from_millis(200);

    let session = SshSession::connect(&options).await.expect("connect");
    let err = session.execute("hang").await.expect_err("should time out");
    assert!(
        matches!(err, SshError::CommandTimeout { .. }),
        "unexpected error: {err}"
    );
}
