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
    /// Number of DNS resolution attempts before giving up, with exponential backoff between
    /// attempts. Applies only to resolution, not to the TCP connect itself.
    pub dns_retries: u32,
    /// Ordered jump hosts. The first is reached directly, then each subsequent host and the
    /// final target are reached through a `direct-tcpip` channel on the preceding connection.
    pub proxy_jump: Vec<ConnectOptions>,
    /// A command to spawn and use as the transport stream instead of a direct TCP connection,
    /// matching OpenSSH's `ProxyCommand`. Only ever consulted for the very first hop reached from
    /// the local machine (`proxy_jump[0]` if a jump chain is configured, otherwise this same
    /// `ConnectOptions` when it is the direct target) -- exactly like real OpenSSH, a
    /// `ProxyCommand` set on a later jump hop is never reachable, since later hops are always
    /// tunnelled through the previous hop's already-established SSH connection, not spawned
    /// locally. Supports `%h`/`%p`/`%r`/`%%` token substitution; see
    /// `substitute_proxy_command_tokens`.
    pub proxy_command: Option<String>,
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
            dns_retries: 3,
            proxy_jump: Vec::new(),
            proxy_command: None,
        }
    }
}

/// Substitutes OpenSSH's `%h` (host), `%p` (port), `%r` (user), and `%%` (literal `%`) tokens in a
/// `ProxyCommand` string. Any other `%`-sequence is left unchanged rather than rejected -- this is
/// a deliberate subset of OpenSSH's token support, not full compatibility.
pub fn substitute_proxy_command_tokens(command: &str, host: &str, port: u16, user: &str) -> String {
    let mut result = String::with_capacity(command.len());
    let mut chars = command.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '%' {
            result.push(c);
            continue;
        }
        match chars.peek() {
            Some('h') => {
                chars.next();
                result.push_str(host);
            }
            Some('p') => {
                chars.next();
                result.push_str(&port.to_string());
            }
            Some('r') => {
                chars.next();
                result.push_str(user);
            }
            Some('%') => {
                chars.next();
                result.push('%');
            }
            _ => result.push('%'),
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_every_supported_token() {
        assert_eq!(
            substitute_proxy_command_tokens(
                "ssh -W %h:%p %r@bastion",
                "target.example.com",
                22,
                "deploy"
            ),
            "ssh -W target.example.com:22 deploy@bastion"
        );
    }

    #[test]
    fn literal_percent_is_preserved() {
        assert_eq!(
            substitute_proxy_command_tokens("echo 100%% done for %h", "host", 22, "user"),
            "echo 100% done for host"
        );
    }

    #[test]
    fn unsupported_tokens_pass_through_unchanged() {
        assert_eq!(
            substitute_proxy_command_tokens("nc %h %p # %L unsupported", "host", 2222, "user"),
            "nc host 2222 # %L unsupported"
        );
    }

    #[test]
    fn command_without_tokens_is_unchanged() {
        assert_eq!(
            substitute_proxy_command_tokens("plain-command", "host", 22, "user"),
            "plain-command"
        );
    }
}
