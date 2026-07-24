use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use russh::client::{self, Handle};
use russh::keys::agent::client::AgentClient;
use russh::keys::agent::AgentIdentity;
use russh::keys::{decode_secret_key, load_secret_key, PrivateKeyWithHashAlg};
use russh::{Channel, ChannelMsg, ChannelOpenFailure, Disconnect};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{copy_bidirectional, AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::error::SshError;
use crate::options::ConnectOptions;

/// Result of running a single command over SSH.
#[derive(Debug, Clone)]
pub struct CommandResult {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
    pub code: Option<u32>,
}

/// One event from a streamed command's channel, in arrival order.
#[derive(Debug, Clone)]
pub enum StreamChunk {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    Exit(u32),
}

/// Outcome of classifying one `ChannelMsg`, shared by both the buffered (`run_command`) and
/// streaming (`execute_streaming`) drain loops so there is exactly one place that knows which
/// channel messages matter and what they mean.
enum DrainStep {
    Chunk(StreamChunk),
    Ignore,
    Done,
}

fn classify_channel_msg(msg: ChannelMsg) -> DrainStep {
    match msg {
        ChannelMsg::Data { data } => DrainStep::Chunk(StreamChunk::Stdout(data.to_vec())),
        ChannelMsg::ExtendedData { data, .. } => {
            DrainStep::Chunk(StreamChunk::Stderr(data.to_vec()))
        }
        ChannelMsg::ExitStatus { exit_status } => DrainStep::Chunk(StreamChunk::Exit(exit_status)),
        ChannelMsg::Close => DrainStep::Done,
        _ => DrainStep::Ignore,
    }
}

/// A single event from an open `PtyChannel`. Once a PTY is attached, a remote shell's stdout and
/// stderr are typically already merged by the remote terminal itself, so both `ChannelMsg::Data`
/// and `ChannelMsg::ExtendedData` collapse into one `Output` variant here -- callers driving an
/// interactive terminal don't need (and can't meaningfully use) the distinction once a PTY is in
/// the picture.
#[derive(Debug, Clone)]
pub enum PtyEvent {
    Output(Vec<u8>),
    Exit(u32),
}

/// An open PTY (or plain interactive-exec) channel, returned by `SshSession::open_pty`. Holds no
/// terminal state of its own -- driving raw mode, local echo, and resize detection is entirely
/// the caller's responsibility; this only carries bytes and requests across the SSH channel.
pub struct PtyChannel {
    channel: Channel<client::Msg>,
}

impl PtyChannel {
    /// Sends local input to the remote pty/command.
    pub async fn send(&self, data: &[u8]) -> Result<(), SshError> {
        self.channel.data_bytes(data.to_vec()).await?;
        Ok(())
    }

    /// Informs the remote pty that the local terminal's size changed.
    pub async fn resize(&self, cols: u16, rows: u16) -> Result<(), SshError> {
        self.channel
            .window_change(u32::from(cols), u32::from(rows), 0, 0)
            .await?;
        Ok(())
    }

    /// Signals no more local input is coming (used for the non-interactive `--interactive`-less
    /// case, where `send` is never called at all).
    pub async fn eof(&self) -> Result<(), SshError> {
        self.channel.eof().await?;
        Ok(())
    }

    /// Waits for the next event. Returns `None` once the channel closes.
    pub async fn recv(&mut self) -> Option<PtyEvent> {
        loop {
            let msg = self.channel.wait().await?;
            match classify_channel_msg(msg) {
                DrainStep::Chunk(StreamChunk::Stdout(data) | StreamChunk::Stderr(data)) => {
                    return Some(PtyEvent::Output(data))
                }
                DrainStep::Chunk(StreamChunk::Exit(code)) => return Some(PtyEvent::Exit(code)),
                DrainStep::Ignore => {}
                DrainStep::Done => return None,
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteForward {
    port: u16,
}

impl RemoteForward {
    pub fn port(&self) -> u16 {
        self.port
    }
}

#[derive(Clone)]
struct ForwardTarget {
    host: String,
    port: u16,
}

type ForwardTargets = Arc<RwLock<HashMap<(String, u32), ForwardTarget>>>;

#[derive(Clone, Default)]
struct ClientHandler {
    forward_targets: ForwardTargets,
}

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    /// jiji does not verify host keys against a known_hosts file (matching the Deno original,
    /// which connects with `StrictHostKeyChecking=no` / `UserKnownHostsFile=/dev/null`).
    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }

    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: Channel<client::Msg>,
        connected_address: &str,
        connected_port: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: russh::client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        let target = self
            .forward_targets
            .read()
            .expect("forward targets lock poisoned")
            .get(&(connected_address.to_string(), connected_port))
            .cloned();
        let Some(target) = target else {
            reply
                .reject(ChannelOpenFailure::AdministrativelyProhibited)
                .await;
            return Ok(());
        };

        // Channel acceptance shares russh's bounded session queue. Awaiting it inside the handler
        // can deadlock that same session when Docker opens several layer connections at once.
        tokio::spawn(async move {
            match TcpStream::connect((target.host.as_str(), target.port)).await {
                Ok(mut local) => {
                    reply.accept().await;
                    let mut remote = channel.into_stream();
                    let _ = copy_bidirectional(&mut remote, &mut local).await;
                }
                Err(_) => {
                    reply.reject(ChannelOpenFailure::ConnectFailed).await;
                }
            }
        });
        Ok(())
    }
}

/// A single authenticated SSH connection to one host.
pub struct SshSession {
    host: String,
    command_timeout: std::time::Duration,
    handle: Handle<ClientHandler>,
    jump_handles: Vec<Handle<ClientHandler>>,
    forward_targets: ForwardTargets,
    /// Holds any `ProxyCommand` child process alive for the session's lifetime. Never read after
    /// construction -- it exists purely for its `Drop` side effect: spawned with
    /// `kill_on_drop(true)`, so the process is killed whichever way this field's `Vec` is dropped,
    /// including on an early return/panic, with no explicit cleanup needed in `close()`.
    #[allow(dead_code)]
    proxy_processes: Vec<tokio::process::Child>,
}

impl SshSession {
    pub async fn connect(options: &ConnectOptions) -> Result<Self, SshError> {
        if options.proxy_jump.is_empty() {
            return Self::connect_direct(options).await;
        }

        let mut jump_handles = Vec::with_capacity(options.proxy_jump.len());
        let (mut current, first_process) =
            connect_first_hop(&options.proxy_jump[0], ClientHandler::default()).await?;
        let proxy_processes: Vec<tokio::process::Child> = first_process.into_iter().collect();
        authenticate(&mut current, &options.proxy_jump[0]).await?;

        for next in options.proxy_jump.iter().skip(1) {
            let channel = open_tunnel(&current, next).await?;
            let mut next_handle =
                connect_stream(channel.into_stream(), next, ClientHandler::default()).await?;
            authenticate(&mut next_handle, next).await?;
            jump_handles.push(current);
            current = next_handle;
        }

        let channel = open_tunnel(&current, options).await?;
        let handler = ClientHandler::default();
        let forward_targets = Arc::clone(&handler.forward_targets);
        let mut handle = connect_stream(channel.into_stream(), options, handler).await?;
        authenticate(&mut handle, options).await?;
        jump_handles.push(current);

        Ok(Self {
            host: options.host.clone(),
            command_timeout: options.command_timeout,
            handle,
            jump_handles,
            forward_targets,
            proxy_processes,
        })
    }

    async fn connect_direct(options: &ConnectOptions) -> Result<Self, SshError> {
        let handler = ClientHandler::default();
        let forward_targets = Arc::clone(&handler.forward_targets);
        let (mut handle, process) = connect_first_hop(options, handler).await?;
        authenticate(&mut handle, options).await?;

        Ok(Self {
            host: options.host.clone(),
            command_timeout: options.command_timeout,
            handle,
            jump_handles: Vec::new(),
            forward_targets,
            proxy_processes: process.into_iter().collect(),
        })
    }

    /// Establishes and authenticates a session over an existing byte stream.
    pub async fn connect_over_stream<R>(
        stream: R,
        options: &ConnectOptions,
    ) -> Result<Self, SshError>
    where
        R: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let handler = ClientHandler::default();
        let forward_targets = Arc::clone(&handler.forward_targets);
        let mut handle = connect_stream(stream, options, handler).await?;
        authenticate(&mut handle, options).await?;

        Ok(Self {
            host: options.host.clone(),
            command_timeout: options.command_timeout,
            handle,
            jump_handles: Vec::new(),
            forward_targets,
            proxy_processes: Vec::new(),
        })
    }

    pub async fn open_direct_tcpip(
        &self,
        host: &str,
        port: u16,
    ) -> Result<russh::Channel<russh::client::Msg>, SshError> {
        self.handle
            .channel_open_direct_tcpip(host, u32::from(port), "127.0.0.1", 0)
            .await
            .map_err(SshError::Protocol)
    }

    pub fn jump_count(&self) -> usize {
        self.jump_handles.len()
    }

    /// Exposes a local TCP service on the remote host's loopback interface for this session.
    ///
    /// Passing `0` for `remote_port` asks the SSH server to allocate an available port.
    pub async fn start_reverse_forward(
        &self,
        local_host: impl Into<String>,
        local_port: u16,
        remote_port: u16,
    ) -> Result<RemoteForward, SshError> {
        const REMOTE_BIND: &str = "127.0.0.1";

        let assigned = self
            .handle
            .tcpip_forward(REMOTE_BIND, u32::from(remote_port))
            .await
            .map_err(|source| SshError::Forward {
                host: self.host.clone(),
                action: format!("open reverse forward on {REMOTE_BIND}:{remote_port}"),
                source,
            })?;
        let assigned = if assigned == 0 && remote_port != 0 {
            u32::from(remote_port)
        } else {
            assigned
        };
        let assigned = u16::try_from(assigned).map_err(|_| SshError::InvalidForwardPort {
            host: self.host.clone(),
            port: assigned,
        })?;
        self.forward_targets
            .write()
            .expect("forward targets lock poisoned")
            .insert(
                (REMOTE_BIND.to_string(), u32::from(assigned)),
                ForwardTarget {
                    host: local_host.into(),
                    port: local_port,
                },
            );
        Ok(RemoteForward { port: assigned })
    }

    pub async fn cancel_reverse_forward(&self, forward: &RemoteForward) -> Result<(), SshError> {
        const REMOTE_BIND: &str = "127.0.0.1";

        self.handle
            .cancel_tcpip_forward(REMOTE_BIND, u32::from(forward.port))
            .await
            .map_err(|source| SshError::Forward {
                host: self.host.clone(),
                action: format!("cancel reverse forward on {REMOTE_BIND}:{}", forward.port),
                source,
            })?;
        self.forward_targets
            .write()
            .expect("forward targets lock poisoned")
            .remove(&(REMOTE_BIND.to_string(), u32::from(forward.port)));
        Ok(())
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn is_connected(&self) -> bool {
        !self.handle.is_closed()
    }

    pub async fn execute(&self, command: &str) -> Result<CommandResult, SshError> {
        self.execute_inner(command, None).await
    }

    pub async fn execute_with_input(
        &self,
        command: &str,
        input: &[u8],
    ) -> Result<CommandResult, SshError> {
        self.execute_inner(command, Some(input)).await
    }

    /// Like `execute`, but delivers stdout/stderr as they arrive instead of buffering the whole
    /// result, through the returned channel. `command_timeout` still bounds the whole call: on
    /// timeout, the channel yields one final `Err(SshError::CommandTimeout)` item, then closes.
    /// The receiving end simply dropping the `Receiver` (e.g. a caller that stopped caring) ends
    /// the background drain loop instead of erroring.
    pub async fn execute_streaming(
        &self,
        command: &str,
    ) -> Result<mpsc::Receiver<Result<StreamChunk, SshError>>, SshError> {
        self.execute_streaming_inner(command, None).await
    }

    pub async fn execute_streaming_with_input(
        &self,
        command: &str,
        input: &[u8],
    ) -> Result<mpsc::Receiver<Result<StreamChunk, SshError>>, SshError> {
        self.execute_streaming_inner(command, Some(input)).await
    }

    async fn execute_streaming_inner(
        &self,
        command: &str,
        input: Option<&[u8]>,
    ) -> Result<mpsc::Receiver<Result<StreamChunk, SshError>>, SshError> {
        let mut channel = self.handle.channel_open_session().await?;
        channel.exec(true, command).await?;
        if let Some(data) = input {
            channel.data_bytes(data.to_vec()).await?;
        }
        channel.eof().await?;

        let (tx, rx) = mpsc::channel(16);
        let command_timeout = self.command_timeout;
        let host = self.host.clone();
        let command = command.to_string();

        tokio::spawn(async move {
            let drained = tokio::time::timeout(command_timeout, async {
                loop {
                    let Some(msg) = channel.wait().await else {
                        break;
                    };
                    match classify_channel_msg(msg) {
                        DrainStep::Chunk(chunk) => {
                            if tx.send(Ok(chunk)).await.is_err() {
                                return;
                            }
                        }
                        DrainStep::Ignore => {}
                        DrainStep::Done => break,
                    }
                }
            })
            .await;

            if drained.is_err() {
                let _ = tx
                    .send(Err(SshError::CommandTimeout {
                        host,
                        command,
                        timeout_secs: command_timeout.as_secs(),
                    }))
                    .await;
            }
        });

        Ok(rx)
    }

    /// Opens a PTY channel: `command` runs that command with a pseudo-terminal attached, `None`
    /// requests an interactive login shell. `jiji-ssh` has no knowledge of the local terminal --
    /// `term`/`cols`/`rows` are the caller's (`jiji-cli`'s) responsibility to determine and keep
    /// updated via `PtyChannel::resize`.
    pub async fn open_pty(
        &self,
        command: Option<&str>,
        term: &str,
        cols: u16,
        rows: u16,
    ) -> Result<PtyChannel, SshError> {
        let channel = self.handle.channel_open_session().await?;
        channel
            .request_pty(true, term, u32::from(cols), u32::from(rows), 0, 0, &[])
            .await
            .map_err(|source| SshError::Pty {
                host: self.host.clone(),
                source,
            })?;
        match command {
            Some(command) => channel.exec(true, command).await?,
            None => channel.request_shell(true).await?,
        }
        Ok(PtyChannel { channel })
    }

    /// Opens a new channel and requests the "sftp" subsystem, returning its raw stream. Used by
    /// the `sftp` module. Returning `impl AsyncRead + AsyncWrite` here (rather than exposing the
    /// channel type itself) means the `sftp` module never needs to name the private
    /// `ClientHandler` type this crate uses internally.
    pub(crate) async fn open_sftp_stream(
        &self,
    ) -> Result<impl AsyncRead + AsyncWrite + Unpin + Send + 'static, SshError> {
        let channel = self.handle.channel_open_session().await?;
        channel.request_subsystem(true, "sftp").await?;
        Ok(channel.into_stream())
    }

    async fn execute_inner(
        &self,
        command: &str,
        input: Option<&[u8]>,
    ) -> Result<CommandResult, SshError> {
        match tokio::time::timeout(self.command_timeout, self.run_command(command, input)).await {
            Ok(result) => result,
            Err(_) => Err(SshError::CommandTimeout {
                host: self.host.clone(),
                command: command.to_string(),
                timeout_secs: self.command_timeout.as_secs(),
            }),
        }
    }

    async fn run_command(
        &self,
        command: &str,
        input: Option<&[u8]>,
    ) -> Result<CommandResult, SshError> {
        let mut channel = self.handle.channel_open_session().await?;
        channel.exec(true, command).await?;

        if let Some(data) = input {
            channel.data_bytes(data.to_vec()).await?;
        }
        channel.eof().await?;

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut code = None;

        loop {
            let Some(msg) = channel.wait().await else {
                break;
            };
            match classify_channel_msg(msg) {
                DrainStep::Chunk(StreamChunk::Stdout(data)) => stdout.extend_from_slice(&data),
                DrainStep::Chunk(StreamChunk::Stderr(data)) => stderr.extend_from_slice(&data),
                DrainStep::Chunk(StreamChunk::Exit(exit_status)) => code = Some(exit_status),
                DrainStep::Ignore => {}
                DrainStep::Done => break,
            }
        }

        Ok(CommandResult {
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
            success: code == Some(0),
            code,
        })
    }

    /// Best-effort disconnect: a failure here just means the connection was already gone, which
    /// isn't an error worth surfacing to the caller.
    pub async fn close(&self) {
        let forwards: Vec<RemoteForward> = self
            .forward_targets
            .read()
            .expect("forward targets lock poisoned")
            .keys()
            .filter_map(|(address, port)| {
                (address == "127.0.0.1")
                    .then(|| u16::try_from(*port).ok())
                    .flatten()
                    .map(|port| RemoteForward { port })
            })
            .collect();
        for forward in forwards {
            let _ = self.cancel_reverse_forward(&forward).await;
        }
        let _ = self
            .handle
            .disconnect(Disconnect::ByApplication, "closing", "en")
            .await;
        for handle in self.jump_handles.iter().rev() {
            let _ = handle
                .disconnect(Disconnect::ByApplication, "closing", "en")
                .await;
        }
    }
}

/// Establishes the very first hop reached from the local machine: either a `ProxyCommand` child
/// process (if `options.proxy_command` is set) or a direct TCP connection. This is the only place
/// `proxy_command` is ever consulted -- see `ConnectOptions::proxy_command`'s doc comment for why
/// later jump hops can't use it.
async fn connect_first_hop(
    options: &ConnectOptions,
    handler: ClientHandler,
) -> Result<(Handle<ClientHandler>, Option<tokio::process::Child>), SshError> {
    match &options.proxy_command {
        Some(command) => {
            let (stream, child) = spawn_proxy_command(command, options)?;
            let handle = connect_stream(stream, options, handler).await?;
            Ok((handle, Some(child)))
        }
        None => {
            let handle = connect_tcp(options, handler).await?;
            Ok((handle, None))
        }
    }
}

/// Spawns `sh -c "<substituted ProxyCommand>"` and returns a duplex stream over its stdio, joined
/// with `tokio::io::join` the same way OpenSSH itself treats `ProxyCommand`'s child process as the
/// transport. `kill_on_drop(true)` on the returned `Child` means the caller only needs to keep it
/// alive for as long as the stream is in use; stderr is inherited so the command's own diagnostics
/// (e.g. a bastion wrapper script failing) reach the user directly.
// `SshError` is shared across every fallible operation in this crate (dozens of call sites use
// `?` against it), so boxing it crate-wide to satisfy this one function's lint would be a much
// larger change than this function warrants; the `Forward` variant is already the largest one and
// predates this function.
#[allow(clippy::result_large_err)]
fn spawn_proxy_command(
    command: &str,
    options: &ConnectOptions,
) -> Result<
    (
        impl AsyncRead + AsyncWrite + Unpin + Send + 'static,
        tokio::process::Child,
    ),
    SshError,
> {
    let substituted = crate::options::substitute_proxy_command_tokens(
        command,
        &options.host,
        options.port,
        &options.user,
    );

    let mut child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(&substituted)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|source| SshError::ProxyCommand {
            host: options.host.clone(),
            command: substituted.clone(),
            source,
        })?;

    let stdin = child.stdin.take().expect("stdin configured as piped above");
    let stdout = child
        .stdout
        .take()
        .expect("stdout configured as piped above");
    let stream = tokio::io::join(stdout, stdin);

    Ok((stream, child))
}

async fn connect_tcp(
    options: &ConnectOptions,
    handler: ClientHandler,
) -> Result<Handle<ClientHandler>, SshError> {
    let config = Arc::new(client::Config::default());

    let connect_future = async {
        let addr = resolve_with_retry(options).await?;
        client::connect(config, addr, handler)
            .await
            .map_err(|source| SshError::Connect {
                host: options.host.clone(),
                port: options.port,
                source,
            })
    };

    match tokio::time::timeout(options.connect_timeout, connect_future).await {
        Ok(result) => result,
        Err(elapsed) => Err(SshError::Connect {
            host: options.host.clone(),
            port: options.port,
            source: russh::Error::from(elapsed),
        }),
    }
}

/// Resolves `options.host:options.port` with exponential backoff (200ms, 400ms, 800ms, ...,
/// capped at 5s between attempts), retrying only resolution failures -- a refused or timed-out
/// TCP connect to an already-resolved address is a different problem, handled by the caller's
/// `connect_timeout`. Runs inside that same timeout, so a misconfigured host still fails bounded.
async fn resolve_with_retry(options: &ConnectOptions) -> Result<SocketAddr, SshError> {
    let target = format!("{}:{}", options.host, options.port);
    let attempts = options.dns_retries.max(1);
    let mut last_error = None;

    for attempt in 0..attempts {
        match tokio::net::lookup_host(&target).await {
            Ok(mut addrs) => match addrs.next() {
                Some(addr) => return Ok(addr),
                None => {
                    last_error = Some(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "DNS resolution returned no addresses",
                    ));
                }
            },
            Err(error) => last_error = Some(error),
        }
        if attempt + 1 < attempts {
            tokio::time::sleep(backoff_delay(attempt)).await;
        }
    }

    Err(SshError::Resolve {
        host: options.host.clone(),
        attempts,
        source: last_error.expect("loop runs at least once since attempts is at least 1"),
    })
}

fn backoff_delay(attempt: u32) -> Duration {
    const BASE: u64 = 200;
    const CAP: u64 = 5000;
    let scaled = BASE.saturating_mul(1u64 << attempt.min(20));
    Duration::from_millis(scaled.min(CAP))
}

async fn connect_stream<R>(
    stream: R,
    options: &ConnectOptions,
    handler: ClientHandler,
) -> Result<Handle<ClientHandler>, SshError>
where
    R: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    tokio::time::timeout(
        options.connect_timeout,
        client::connect_stream(Arc::new(client::Config::default()), stream, handler),
    )
    .await
    .map_err(|elapsed| SshError::Connect {
        host: options.host.clone(),
        port: options.port,
        source: russh::Error::from(elapsed),
    })?
    .map_err(|source| SshError::Connect {
        host: options.host.clone(),
        port: options.port,
        source,
    })
}

async fn open_tunnel(
    handle: &Handle<ClientHandler>,
    target: &ConnectOptions,
) -> Result<russh::Channel<russh::client::Msg>, SshError> {
    handle
        .channel_open_direct_tcpip(target.host.as_str(), u32::from(target.port), "127.0.0.1", 0)
        .await
        .map_err(SshError::Protocol)
}

async fn authenticate(
    handle: &mut Handle<ClientHandler>,
    options: &ConnectOptions,
) -> Result<(), SshError> {
    let has_explicit_keys = !options.keys.is_empty() || !options.key_data.is_empty();

    if has_explicit_keys {
        let mut attempted = Vec::new();

        for path in &options.keys {
            let key =
                load_secret_key(path, options.key_passphrase.as_deref()).map_err(|source| {
                    SshError::KeyLoad {
                        path: path.display().to_string(),
                        source,
                    }
                })?;
            attempted.push(path.display().to_string());
            if try_publickey(handle, options, key).await? {
                return Ok(());
            }
        }

        for (i, data) in options.key_data.iter().enumerate() {
            let key =
                decode_secret_key(data, options.key_passphrase.as_deref()).map_err(|source| {
                    SshError::KeyLoad {
                        path: format!("key_data[{i}]"),
                        source,
                    }
                })?;
            attempted.push(format!("key_data[{i}]"));
            if try_publickey(handle, options, key).await? {
                return Ok(());
            }
        }

        if !options.keys_only && std::env::var_os("SSH_AUTH_SOCK").is_some() {
            attempted.push("ssh-agent".to_string());
            if try_agent(handle, options).await? {
                return Ok(());
            }
        }

        return Err(SshError::Auth {
            host: options.host.clone(),
            user: options.user.clone(),
            reason: format!(
                "none of the configured credentials were accepted (tried: {})",
                attempted.join(", ")
            ),
        });
    }

    if std::env::var_os("SSH_AUTH_SOCK").is_none() {
        return Err(SshError::AgentUnavailable);
    }

    if try_agent(handle, options).await? {
        Ok(())
    } else {
        Err(SshError::Auth {
            host: options.host.clone(),
            user: options.user.clone(),
            reason: "ssh-agent did not have an accepted key for this host".to_string(),
        })
    }
}

async fn try_publickey(
    handle: &mut Handle<ClientHandler>,
    options: &ConnectOptions,
    key: russh::keys::PrivateKey,
) -> Result<bool, SshError> {
    let hash_alg = handle.best_supported_rsa_hash().await?.flatten();
    let result = handle
        .authenticate_publickey(
            &options.user,
            PrivateKeyWithHashAlg::new(Arc::new(key), hash_alg),
        )
        .await?;
    Ok(result.success())
}

async fn try_agent(
    handle: &mut Handle<ClientHandler>,
    options: &ConnectOptions,
) -> Result<bool, SshError> {
    let mut agent = AgentClient::connect_env()
        .await
        .map_err(|err| SshError::Agent(err.to_string()))?;
    let identities = agent
        .request_identities()
        .await
        .map_err(|err| SshError::Agent(err.to_string()))?;

    for identity in identities {
        let AgentIdentity::PublicKey { key, .. } = identity else {
            continue;
        };
        let hash_alg = handle.best_supported_rsa_hash().await?.flatten();
        let result = handle
            .authenticate_publickey_with(&options.user, key, hash_alg, &mut agent)
            .await
            .map_err(|err| SshError::Agent(err.to_string()))?;
        if result.success() {
            return Ok(true);
        }
    }

    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::ConnectOptions;

    #[test]
    fn backoff_delay_grows_and_caps_at_five_seconds() {
        assert_eq!(backoff_delay(0), Duration::from_millis(200));
        assert_eq!(backoff_delay(1), Duration::from_millis(400));
        assert_eq!(backoff_delay(2), Duration::from_millis(800));
        assert_eq!(backoff_delay(3), Duration::from_millis(1600));
        assert_eq!(backoff_delay(10), Duration::from_millis(5000));
    }

    #[tokio::test]
    async fn resolve_with_retry_exhausts_configured_attempts_on_unresolvable_host() {
        let mut options = ConnectOptions::new("this-host-should-never-resolve.invalid", "tester");
        options.dns_retries = 2;
        let error = resolve_with_retry(&options)
            .await
            .expect_err("should fail to resolve");
        match error {
            SshError::Resolve { host, attempts, .. } => {
                assert_eq!(host, "this-host-should-never-resolve.invalid");
                assert_eq!(attempts, 2);
            }
            other => panic!("expected SshError::Resolve, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resolve_with_retry_succeeds_immediately_for_a_loopback_address() {
        let mut options = ConnectOptions::new("127.0.0.1", "tester");
        options.port = 22;
        let addr = resolve_with_retry(&options)
            .await
            .expect("should resolve loopback");
        assert_eq!(addr.ip().to_string(), "127.0.0.1");
    }
}
