use thiserror::Error;

#[derive(Debug, Error)]
pub enum SshError {
    #[error("Failed to connect to {host}:{port}: {source}")]
    Connect {
        host: String,
        port: u16,
        #[source]
        source: russh::Error,
    },

    #[error(
        "Could not resolve host {host} after {attempts} attempt(s): {source}. Verify the hostname and DNS configuration, then retry."
    )]
    Resolve {
        host: String,
        attempts: u32,
        #[source]
        source: std::io::Error,
    },

    #[error(
        "Could not start ProxyCommand `{command}` for {host}: {source}. Verify the command is on PATH and runs standalone, then retry."
    )]
    ProxyCommand {
        host: String,
        command: String,
        #[source]
        source: std::io::Error,
    },

    #[error("Authentication to {user}@{host} failed: {reason}")]
    Auth {
        host: String,
        user: String,
        reason: String,
    },

    #[error("Failed to read SSH key '{path}': {source}")]
    KeyLoad {
        path: String,
        #[source]
        source: russh::keys::Error,
    },

    #[error(
        "SSH_AUTH_SOCK is not set. Start ssh-agent, or set `keys` in the ssh config to authenticate without one."
    )]
    AgentUnavailable,

    #[error("ssh-agent error: {0}")]
    Agent(String),

    #[error("Command on {host} exceeded the {timeout_secs}s command_timeout: {command}")]
    CommandTimeout {
        host: String,
        command: String,
        timeout_secs: u64,
    },

    #[error(
        "Could not {action} through SSH host {host}: {source}. Verify `AllowTcpForwarding yes`, ensure the remote port is available, and retry."
    )]
    Forward {
        host: String,
        action: String,
        #[source]
        source: russh::Error,
    },

    #[error(
        "SSH host {host} returned invalid allocated forwarding port {port}. Configure a fixed unprivileged port and retry."
    )]
    InvalidForwardPort { host: String, port: u32 },

    #[error("Could not start a PTY session on {host}: {source}")]
    Pty {
        host: String,
        #[source]
        source: russh::Error,
    },

    /// `reason` is a stringified underlying error rather than a `#[source]`-chained one: a single
    /// SFTP call can fail from either `russh_sftp::client::Error` (the SFTP protocol layer) or
    /// `std::io::Error` (local file I/O, or the remote `File` handle's `AsyncWrite`/`AsyncRead`
    /// impl surfacing a transport failure as `io::Error`) -- unifying two heterogeneous error
    /// types behind one variant, the same reason `Agent(String)` above does the same thing.
    #[error("SFTP {path} on {host} failed: {reason}")]
    Sftp {
        host: String,
        path: String,
        reason: String,
    },

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Protocol(#[from] russh::Error),
}
