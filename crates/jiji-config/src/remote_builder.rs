//! Typed `builder.remote` URI parsing. The config schema stores `remote` as a raw
//! `Option<String>` (`ssh://[user@]hostname[:port]`); this module is the single place that
//! string gets turned into something a connection layer can use. Hand-rolled rather than
//! pulling in the `url` crate: no URI-parsing dependency exists anywhere in this workspace, and
//! `url::Url` is far more permissive than the narrow OpenSSH-style shape this field accepts.

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteBuilder {
    pub host: String,
    pub user: Option<String>,
    pub port: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RemoteBuilderUriError {
    #[error("'builder.remote' is empty. Use `ssh://[user@]hostname[:port]`.")]
    Empty,

    #[error("'builder.remote' must start with `ssh://`. Use `ssh://[user@]hostname[:port]`.")]
    InvalidScheme,

    #[error(
        "'builder.remote' may not include a password. Use `ssh://[user@]hostname[:port]` and configure authentication under `ssh:`."
    )]
    PasswordNotAllowed,

    #[error("'builder.remote' may not include a {0}. Use `ssh://[user@]hostname[:port]`.")]
    UnsupportedComponent(&'static str),

    #[error("'builder.remote' is missing a hostname. Use `ssh://[user@]hostname[:port]`.")]
    EmptyHost,

    #[error(
        "'builder.remote' has an empty user before '@'. Use `ssh://[user@]hostname[:port]` or omit the user."
    )]
    EmptyUser,

    #[error(
        "'builder.remote' has port 0, which is not a valid port. Use 1-65535 or omit the port."
    )]
    ZeroPort,

    #[error("'builder.remote' has an invalid port '{0}'. Use a number from 1-65535.")]
    InvalidPort(String),
}

const SCHEME_PREFIX: &str = "ssh://";

pub fn parse_remote_builder_uri(raw: &str) -> Result<RemoteBuilder, RemoteBuilderUriError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(RemoteBuilderUriError::Empty);
    }
    let rest = raw
        .strip_prefix(SCHEME_PREFIX)
        .ok_or(RemoteBuilderUriError::InvalidScheme)?;
    if rest.is_empty() {
        return Err(RemoteBuilderUriError::EmptyHost);
    }

    if let Some(index) = rest.find(['/', '?', '#']) {
        let kind = match rest.as_bytes()[index] {
            b'/' => "path",
            b'?' => "query string",
            _ => "fragment",
        };
        return Err(RemoteBuilderUriError::UnsupportedComponent(kind));
    }

    let (userinfo, host_port) = match rest.rsplit_once('@') {
        Some((userinfo, host_port)) => (Some(userinfo), host_port),
        None => (None, rest),
    };

    let user = match userinfo {
        Some(userinfo) => {
            if userinfo.contains(':') {
                return Err(RemoteBuilderUriError::PasswordNotAllowed);
            }
            if userinfo.is_empty() {
                return Err(RemoteBuilderUriError::EmptyUser);
            }
            Some(userinfo.to_string())
        }
        None => None,
    };

    let (host, port) = parse_host_port(host_port)?;
    if host.is_empty() {
        return Err(RemoteBuilderUriError::EmptyHost);
    }

    Ok(RemoteBuilder { host, user, port })
}

/// Splits `host[:port]`, where `host` may be a bracketed IPv6 literal (`[::1]` or `[::1]:2222`).
fn parse_host_port(value: &str) -> Result<(String, Option<u16>), RemoteBuilderUriError> {
    if let Some(rest) = value.strip_prefix('[') {
        let (host, after) = rest
            .split_once(']')
            .ok_or(RemoteBuilderUriError::EmptyHost)?;
        let port = match after.strip_prefix(':') {
            Some(port_str) => Some(parse_port(port_str)?),
            None if after.is_empty() => None,
            None => return Err(RemoteBuilderUriError::EmptyHost),
        };
        return Ok((host.to_string(), port));
    }

    match value.rsplit_once(':') {
        Some((host, port_str)) => Ok((host.to_string(), Some(parse_port(port_str)?))),
        None => Ok((value.to_string(), None)),
    }
}

fn parse_port(value: &str) -> Result<u16, RemoteBuilderUriError> {
    let port: u32 = value
        .parse()
        .map_err(|_| RemoteBuilderUriError::InvalidPort(value.to_string()))?;
    if port == 0 {
        return Err(RemoteBuilderUriError::ZeroPort);
    }
    u16::try_from(port).map_err(|_| RemoteBuilderUriError::InvalidPort(value.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_host_only() {
        let parsed = parse_remote_builder_uri("ssh://build-server.example.com").unwrap();
        assert_eq!(parsed.host, "build-server.example.com");
        assert_eq!(parsed.user, None);
        assert_eq!(parsed.port, None);
    }

    #[test]
    fn parses_user_and_port() {
        let parsed = parse_remote_builder_uri("ssh://builder@192.168.1.50:2222").unwrap();
        assert_eq!(parsed.host, "192.168.1.50");
        assert_eq!(parsed.user.as_deref(), Some("builder"));
        assert_eq!(parsed.port, Some(2222));
    }

    #[test]
    fn parses_bracketed_ipv6_without_port() {
        let parsed = parse_remote_builder_uri("ssh://[2001:db8::1]").unwrap();
        assert_eq!(parsed.host, "2001:db8::1");
        assert_eq!(parsed.port, None);
    }

    #[test]
    fn parses_bracketed_ipv6_with_user_and_port() {
        let parsed = parse_remote_builder_uri("ssh://root@[::1]:2200").unwrap();
        assert_eq!(parsed.host, "::1");
        assert_eq!(parsed.user.as_deref(), Some("root"));
        assert_eq!(parsed.port, Some(2200));
    }

    #[test]
    fn rejects_empty_value() {
        assert_eq!(
            parse_remote_builder_uri(""),
            Err(RemoteBuilderUriError::Empty)
        );
        assert_eq!(
            parse_remote_builder_uri("   "),
            Err(RemoteBuilderUriError::Empty)
        );
    }

    #[test]
    fn rejects_missing_or_wrong_scheme() {
        assert_eq!(
            parse_remote_builder_uri("build-server.example.com"),
            Err(RemoteBuilderUriError::InvalidScheme)
        );
        assert_eq!(
            parse_remote_builder_uri("http://build-server.example.com"),
            Err(RemoteBuilderUriError::InvalidScheme)
        );
    }

    #[test]
    fn rejects_password() {
        assert_eq!(
            parse_remote_builder_uri("ssh://user:hunter2@host"),
            Err(RemoteBuilderUriError::PasswordNotAllowed)
        );
    }

    #[test]
    fn rejects_path_query_fragment() {
        assert_eq!(
            parse_remote_builder_uri("ssh://host/path"),
            Err(RemoteBuilderUriError::UnsupportedComponent("path"))
        );
        assert_eq!(
            parse_remote_builder_uri("ssh://host?query=1"),
            Err(RemoteBuilderUriError::UnsupportedComponent("query string"))
        );
        assert_eq!(
            parse_remote_builder_uri("ssh://host#frag"),
            Err(RemoteBuilderUriError::UnsupportedComponent("fragment"))
        );
    }

    #[test]
    fn rejects_empty_host() {
        assert_eq!(
            parse_remote_builder_uri("ssh://"),
            Err(RemoteBuilderUriError::EmptyHost)
        );
        assert_eq!(
            parse_remote_builder_uri("ssh://user@"),
            Err(RemoteBuilderUriError::EmptyHost)
        );
    }

    #[test]
    fn rejects_empty_user() {
        assert_eq!(
            parse_remote_builder_uri("ssh://@host"),
            Err(RemoteBuilderUriError::EmptyUser)
        );
    }

    #[test]
    fn rejects_zero_and_invalid_port() {
        assert_eq!(
            parse_remote_builder_uri("ssh://host:0"),
            Err(RemoteBuilderUriError::ZeroPort)
        );
        assert_eq!(
            parse_remote_builder_uri("ssh://host:notaport"),
            Err(RemoteBuilderUriError::InvalidPort("notaport".to_string()))
        );
        assert_eq!(
            parse_remote_builder_uri("ssh://host:99999"),
            Err(RemoteBuilderUriError::InvalidPort("99999".to_string()))
        );
    }

    #[test]
    fn error_messages_are_actionable() {
        let error = parse_remote_builder_uri("ssh://user:pass@host").unwrap_err();
        assert!(error.to_string().contains("builder.remote"));
        assert!(error.to_string().contains("password"));
    }
}
