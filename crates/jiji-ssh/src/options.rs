use std::path::PathBuf;
use std::time::Duration;

/// Connection and authentication settings for a single SSH host.
///
/// Deliberately decoupled from `jiji_config::Ssh`/`NamedServer`: this crate has no dependency on
/// `jiji-config` so it stays reusable and independently testable. Callers build a
/// `ConnectOptions` from whatever configuration source they have.
#[derive(Debug, Clone)]
pub struct ConnectOptions {
    pub host: String,
    pub port: u16,
    pub user: String,
    /// Private key file paths, tried in order.
    pub keys: Vec<PathBuf>,
    /// Inline private key material (e.g. loaded from an environment variable), tried in order
    /// after `keys`.
    pub key_data: Vec<String>,
    /// Passphrase applied to every key in `keys`/`key_data` that needs one.
    pub key_passphrase: Option<String>,
    /// If true, never fall back to ssh-agent even if `SSH_AUTH_SOCK` is set.
    pub keys_only: bool,
    pub connect_timeout: Duration,
    pub command_timeout: Duration,
    /// Ordered jump hosts. The first is reached directly, then each subsequent host and the
    /// final target are reached through a `direct-tcpip` channel on the preceding connection.
    pub proxy_jump: Vec<ConnectOptions>,
}

impl ConnectOptions {
    pub fn new(host: impl Into<String>, user: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            port: 22,
            user: user.into(),
            keys: Vec::new(),
            key_data: Vec::new(),
            key_passphrase: None,
            keys_only: false,
            connect_timeout: Duration::from_secs(30),
            command_timeout: Duration::from_secs(300),
            proxy_jump: Vec::new(),
        }
    }
}
