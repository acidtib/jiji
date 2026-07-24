use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use russh::client::{self, Handle};
use russh::keys::agent::client::AgentClient;
use russh::keys::agent::AgentIdentity;
use russh::keys::{decode_secret_key, load_secret_key, PrivateKeyWithHashAlg};
use russh::{Channel, ChannelMsg, ChannelOpenFailure, Disconnect};
use tokio::io::{copy_bidirectional, AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

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
}

impl SshSession {
    pub async fn connect(options: &ConnectOptions) -> Result<Self, SshError> {
        if options.proxy_jump.is_empty() {
            return Self::connect_direct(options).await;
        }

        let mut jump_handles = Vec::with_capacity(options.proxy_jump.len());
        let mut current = connect_tcp(&options.proxy_jump[0], ClientHandler::default()).await?;
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
        })
    }

    async fn connect_direct(options: &ConnectOptions) -> Result<Self, SshError> {
        let handler = ClientHandler::default();
        let forward_targets = Arc::clone(&handler.forward_targets);
        let mut handle = connect_tcp(options, handler).await?;
        authenticate(&mut handle, options).await?;

        Ok(Self {
            host: options.host.clone(),
            command_timeout: options.command_timeout,
            handle,
            jump_handles: Vec::new(),
            forward_targets,
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

        while let Some(msg) = channel.wait().await {
            match msg {
                ChannelMsg::Data { data } => stdout.extend_from_slice(&data),
                ChannelMsg::ExtendedData { data, .. } => stderr.extend_from_slice(&data),
                ChannelMsg::ExitStatus { exit_status } => code = Some(exit_status),
                ChannelMsg::Close => break,
                _ => {}
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

async fn connect_tcp(
    options: &ConnectOptions,
    handler: ClientHandler,
) -> Result<Handle<ClientHandler>, SshError> {
    let config = Arc::new(client::Config::default());
    let addr = (options.host.as_str(), options.port);

    tokio::time::timeout(
        options.connect_timeout,
        client::connect(config, addr, handler),
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
