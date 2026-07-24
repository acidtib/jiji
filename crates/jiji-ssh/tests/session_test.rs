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
use tokio::io::{copy_bidirectional, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

use jiji_ssh::{ConnectOptions, SshError, SshSession, StreamChunk};

type RemoteForwardTasks = Arc<Mutex<HashMap<(String, u32), JoinHandle<()>>>>;

#[derive(Clone)]
struct TestServer {
    authorized_key: PublicKey,
    pending: Arc<Mutex<HashMap<ChannelId, String>>>,
    stdin: Arc<Mutex<HashMap<ChannelId, Vec<u8>>>>,
    allow_remote_forward: bool,
    remote_forwards: RemoteForwardTasks,
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

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<server::Msg>,
        host_to_connect: &str,
        port_to_connect: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let destination = format!("{host_to_connect}:{port_to_connect}");
        match TcpStream::connect(destination).await {
            Ok(mut stream) => {
                reply.accept().await;
                tokio::spawn(async move {
                    let mut channel = channel.into_stream();
                    let _ = copy_bidirectional(&mut channel, &mut stream).await;
                });
            }
            Err(_) => {
                reply.reject(russh::ChannelOpenFailure::ConnectFailed).await;
            }
        }
        Ok(())
    }

    async fn tcpip_forward(
        &mut self,
        address: &str,
        port: &mut u32,
        session: &mut Session,
    ) -> Result<bool, Self::Error> {
        if !self.allow_remote_forward {
            return Ok(false);
        }

        let listener = match TcpListener::bind((address, *port as u16)).await {
            Ok(listener) => listener,
            Err(_) => return Ok(false),
        };
        *port = u32::from(listener.local_addr()?.port());
        let key = (address.to_string(), *port);
        let connected_address = address.to_string();
        let connected_port = *port;
        let handle = session.handle();
        let task = tokio::spawn(async move {
            while let Ok((mut tcp, originator)) = listener.accept().await {
                let Ok(channel) = handle
                    .channel_open_forwarded_tcpip(
                        connected_address.clone(),
                        connected_port,
                        originator.ip().to_string(),
                        u32::from(originator.port()),
                    )
                    .await
                else {
                    continue;
                };
                tokio::spawn(async move {
                    let mut ssh = channel.into_stream();
                    let _ = copy_bidirectional(&mut tcp, &mut ssh).await;
                });
            }
        });
        self.remote_forwards
            .lock()
            .expect("remote forwards mutex poisoned")
            .insert(key, task);
        Ok(true)
    }

    async fn cancel_tcpip_forward(
        &mut self,
        address: &str,
        port: u32,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        let task = self
            .remote_forwards
            .lock()
            .expect("remote forwards mutex poisoned")
            .remove(&(address.to_string(), port));
        if let Some(task) = task {
            task.abort();
            Ok(true)
        } else {
            Ok(false)
        }
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
            "multi-chunk" => {
                session.data(channel, "chunk-1\n".to_string())?;
                session.data(channel, "chunk-2\n".to_string())?;
                session.extended_data(channel, 1, "err-chunk\n".to_string())?;
                session.exit_status_request(channel, 0)?;
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
    spawn_test_server_with_forwarding(authorized_key, true).await
}

async fn spawn_test_server_with_forwarding(
    authorized_key: PublicKey,
    allow_remote_forward: bool,
) -> SocketAddr {
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
        allow_remote_forward,
        remote_forwards: Arc::new(Mutex::new(HashMap::new())),
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

#[tokio::test]
async fn connects_and_executes_through_a_proxy_jump() {
    let client_key = generate_client_key();
    let target_addr = spawn_test_server(client_key.public_key().clone()).await;
    let jump_addr = spawn_test_server(client_key.public_key().clone()).await;
    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = write_key_file(&dir, &client_key);

    let mut options = base_options(target_addr);
    options.keys = vec![key_path.clone()];
    options.keys_only = true;

    let mut jump = base_options(jump_addr);
    jump.keys = vec![key_path];
    jump.keys_only = true;
    options.proxy_jump = vec![jump];

    let session = SshSession::connect(&options)
        .await
        .expect("connect through jump");
    assert_eq!(session.jump_count(), 1);

    let result = session.execute("hostname").await.expect("execute");
    assert!(result.success);
    assert!(result.stdout.contains("ran: hostname"));
    session.close().await;
}

#[tokio::test]
async fn reverse_forward_relays_to_a_local_tcp_service_and_can_be_cancelled() {
    let client_key = generate_client_key();
    let ssh_addr = spawn_test_server(client_key.public_key().clone()).await;
    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = write_key_file(&dir, &client_key);
    let local = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind local service");
    let local_port = local.local_addr().expect("local address").port();
    tokio::spawn(async move {
        let (mut stream, _) = local.accept().await.expect("accept local connection");
        let mut request = [0_u8; 4];
        stream.read_exact(&mut request).await.expect("read request");
        assert_eq!(&request, b"ping");
        stream.write_all(b"pong").await.expect("write response");
    });

    let mut options = base_options(ssh_addr);
    options.keys = vec![key_path];
    options.keys_only = true;
    let session = SshSession::connect(&options).await.expect("connect");

    let forward = session
        .start_reverse_forward("127.0.0.1", local_port, 0)
        .await
        .expect("start reverse forward");
    assert_ne!(forward.port(), 0);

    let mut remote = TcpStream::connect(("127.0.0.1", forward.port()))
        .await
        .expect("connect to remote forward");
    remote.write_all(b"ping").await.expect("send request");
    let mut response = [0_u8; 4];
    remote
        .read_exact(&mut response)
        .await
        .expect("read response");
    assert_eq!(&response, b"pong");
    drop(remote);

    session
        .cancel_reverse_forward(&forward)
        .await
        .expect("cancel reverse forward");
    assert!(TcpStream::connect(("127.0.0.1", forward.port()))
        .await
        .is_err());

    let reserved = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("reserve fixed remote port");
    let fixed_remote_port = reserved.local_addr().expect("reserved address").port();
    drop(reserved);
    let close_forward = session
        .start_reverse_forward("127.0.0.1", local_port, fixed_remote_port)
        .await
        .expect("start forward for close cleanup");
    assert_eq!(close_forward.port(), fixed_remote_port);
    session.close().await;
    assert!(TcpStream::connect(("127.0.0.1", close_forward.port()))
        .await
        .is_err());
}

#[tokio::test]
async fn reverse_forward_relays_a_payload_larger_than_the_ssh_channel_window() {
    const PAYLOAD_SIZE: usize = 8 * 1024 * 1024;

    let client_key = generate_client_key();
    let ssh_addr = spawn_test_server(client_key.public_key().clone()).await;
    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = write_key_file(&dir, &client_key);
    let local = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind local service");
    let local_port = local.local_addr().expect("local address").port();
    tokio::spawn(async move {
        let (mut stream, _) = local.accept().await.expect("accept local connection");
        stream
            .write_all(&vec![0x5a; PAYLOAD_SIZE])
            .await
            .expect("write large response");
        stream.shutdown().await.expect("close local response");
    });

    let mut options = base_options(ssh_addr);
    options.keys = vec![key_path];
    options.keys_only = true;
    let session = SshSession::connect(&options).await.expect("connect");
    let forward = session
        .start_reverse_forward("127.0.0.1", local_port, 0)
        .await
        .expect("start reverse forward");

    let mut remote = TcpStream::connect(("127.0.0.1", forward.port()))
        .await
        .expect("connect to remote forward");
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), remote.read_to_end(&mut response))
        .await
        .expect("large response timed out")
        .expect("read large response");
    assert_eq!(response.len(), PAYLOAD_SIZE);
    assert!(response.iter().all(|byte| *byte == 0x5a));
    session.close().await;
}

#[tokio::test]
async fn denied_reverse_forward_has_an_actionable_error() {
    let client_key = generate_client_key();
    let ssh_addr = spawn_test_server_with_forwarding(client_key.public_key().clone(), false).await;
    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = write_key_file(&dir, &client_key);
    let mut options = base_options(ssh_addr);
    options.keys = vec![key_path];
    options.keys_only = true;
    let session = SshSession::connect(&options).await.expect("connect");

    let error = session
        .start_reverse_forward("127.0.0.1", 31270, 31270)
        .await
        .expect_err("forward should be denied");
    assert!(error.to_string().contains("AllowTcpForwarding yes"));
    session.close().await;
}

#[tokio::test]
async fn occupied_remote_port_is_rejected_with_an_actionable_error() {
    let client_key = generate_client_key();
    let ssh_addr = spawn_test_server(client_key.public_key().clone()).await;
    let occupied = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind occupied port");
    let occupied_port = occupied.local_addr().expect("occupied address").port();
    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = write_key_file(&dir, &client_key);
    let mut options = base_options(ssh_addr);
    options.keys = vec![key_path];
    options.keys_only = true;
    let session = SshSession::connect(&options).await.expect("connect");

    let error = session
        .start_reverse_forward("127.0.0.1", 31270, occupied_port)
        .await
        .expect_err("occupied forward should be denied");
    assert!(error.to_string().contains("AllowTcpForwarding yes"));
    session.close().await;
    drop(occupied);
}

#[tokio::test]
async fn execute_streaming_delivers_chunks_as_they_arrive_not_as_one_aggregate() {
    let client_key = generate_client_key();
    let addr = spawn_test_server(client_key.public_key().clone()).await;
    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = write_key_file(&dir, &client_key);
    let mut options = base_options(addr);
    options.keys = vec![key_path];
    options.keys_only = true;
    let session = SshSession::connect(&options).await.expect("connect");

    let mut receiver = session
        .execute_streaming("multi-chunk")
        .await
        .expect("start streaming command");

    let mut stdout_chunks = Vec::new();
    let mut stderr_chunks = Vec::new();
    let mut exit_code = None;
    while let Some(item) = receiver.recv().await {
        match item.expect("no error expected") {
            StreamChunk::Stdout(data) => {
                stdout_chunks.push(String::from_utf8_lossy(&data).into_owned())
            }
            StreamChunk::Stderr(data) => {
                stderr_chunks.push(String::from_utf8_lossy(&data).into_owned())
            }
            StreamChunk::Exit(code) => exit_code = Some(code),
        }
    }

    // Two separate stdout chunks, not one merged string: proves data is forwarded as it arrives.
    assert_eq!(
        stdout_chunks,
        vec!["chunk-1\n".to_string(), "chunk-2\n".to_string()]
    );
    assert_eq!(stderr_chunks, vec!["err-chunk\n".to_string()]);
    assert_eq!(exit_code, Some(0));
    session.close().await;
}

#[tokio::test]
async fn execute_streaming_reports_command_timeout_through_the_channel() {
    let client_key = generate_client_key();
    let addr = spawn_test_server(client_key.public_key().clone()).await;
    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = write_key_file(&dir, &client_key);
    let mut options = base_options(addr);
    options.keys = vec![key_path];
    options.keys_only = true;
    options.command_timeout = Duration::from_millis(100);
    let session = SshSession::connect(&options).await.expect("connect");

    let mut receiver = session
        .execute_streaming("hang")
        .await
        .expect("start streaming command");

    let item = receiver
        .recv()
        .await
        .expect("channel should yield a timeout error before closing");
    assert!(matches!(item, Err(SshError::CommandTimeout { .. })));
    assert!(receiver.recv().await.is_none());
    session.close().await;
}

#[tokio::test]
async fn connects_and_executes_through_a_proxy_command() {
    // Requires `socat` on PATH to relay the ProxyCommand's stdio to the mock server's TCP
    // socket. Not a hard dependency of the crate itself (ProxyCommand can be any command a user
    // configures) -- skip gracefully on environments that don't have it rather than failing CI.
    if std::process::Command::new("socat")
        .arg("-V")
        .output()
        .is_err()
    {
        eprintln!("skipping connects_and_executes_through_a_proxy_command: socat not on PATH");
        return;
    }

    let client_key = generate_client_key();
    let addr = spawn_test_server(client_key.public_key().clone()).await;
    let dir = tempfile::tempdir().expect("create temp dir");
    let key_path = write_key_file(&dir, &client_key);

    // The host in `options` is deliberately not the literal connectable address: %h/%p in the
    // ProxyCommand are substituted from `options.host`/`options.port`, proving substitution (not
    // a hardcoded address) drives the actual connection.
    let mut options = ConnectOptions::new(addr.ip().to_string(), "tester");
    options.port = addr.port();
    options.connect_timeout = Duration::from_secs(5);
    options.command_timeout = Duration::from_secs(5);
    options.keys = vec![key_path];
    options.keys_only = true;
    options.proxy_command = Some("socat - TCP:%h:%p".to_string());

    let session = SshSession::connect(&options)
        .await
        .expect("connect through ProxyCommand");
    let result = session.execute("hostname").await.expect("execute");
    assert!(result.success);
    assert!(result.stdout.contains("ran: hostname"));
    session.close().await;
}
