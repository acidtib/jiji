use std::sync::Arc;

use russh::client::{self, Handle};
use russh::keys::agent::client::AgentClient;
use russh::keys::agent::AgentIdentity;
use russh::keys::{decode_secret_key, load_secret_key, PrivateKeyWithHashAlg};
use russh::{ChannelMsg, Disconnect};

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

struct ClientHandler;

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
}

/// A single authenticated SSH connection to one host.
pub struct SshSession {
    host: String,
    command_timeout: std::time::Duration,
    handle: Handle<ClientHandler>,
}

impl SshSession {
    pub async fn connect(options: &ConnectOptions) -> Result<Self, SshError> {
        let config = Arc::new(client::Config::default());
        let addr = (options.host.as_str(), options.port);

        let mut handle = tokio::time::timeout(
            options.connect_timeout,
            client::connect(config, addr, ClientHandler),
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
        })?;

        authenticate(&mut handle, options).await?;

        Ok(Self {
            host: options.host.clone(),
            command_timeout: options.command_timeout,
            handle,
        })
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
        let _ = self
            .handle
            .disconnect(Disconnect::ByApplication, "closing", "en")
            .await;
    }
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
