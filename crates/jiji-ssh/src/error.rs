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
        "SSH_AUTH_SOCK is not set. Start ssh-agent, or set `keys`/`key_data` in the ssh config to authenticate without one."
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

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Protocol(#[from] russh::Error),
}
