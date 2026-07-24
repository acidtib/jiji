use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use jiji_config::{NamedServer, Ssh, SshConfigFiles};
use jiji_ssh::ConnectOptions;
use ssh2_config::{HostParams, ParseRule, SshConfig};

const DEFAULT_PORT: u16 = 22;
const DEFAULT_CONNECT_TIMEOUT: u32 = 30;

pub fn connect_options(
    name: &str,
    server: &NamedServer,
    ssh: &Ssh,
) -> anyhow::Result<ConnectOptions> {
    let config = load_ssh_config(&ssh.config)?;
    let params = config.as_ref().map(|config| config.query(&server.host));

    reject_proxy_command(name, params.as_ref())?;

    let user = server
        .user
        .clone()
        .or_else(|| ssh.user.clone())
        .or_else(|| params.as_ref().and_then(|params| params.user.clone()))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Server '{name}' has no SSH user configured. Set `user:` on the server, add top-level `ssh.user:`, or enable `ssh.config` with a matching `User`."
            )
        })?;

    let mut options = ConnectOptions::new(
        params
            .as_ref()
            .and_then(|params| params.host_name.clone())
            .unwrap_or_else(|| server.host.clone()),
        user,
    );
    options.port = server.port.unwrap_or_else(|| {
        if ssh.port != DEFAULT_PORT {
            ssh.port
        } else {
            params
                .as_ref()
                .and_then(|params| params.port)
                .unwrap_or(DEFAULT_PORT)
        }
    });
    options.keys = configured_keys(server, ssh)
        .or_else(|| {
            params
                .as_ref()
                .and_then(|params| params.identity_file.clone())
        })
        .unwrap_or_default();
    options.key_data = server
        .key_data
        .clone()
        .or_else(|| ssh.key_data.clone())
        .unwrap_or_default();
    options.key_passphrase = server
        .key_passphrase
        .clone()
        .or_else(|| ssh.key_passphrase.clone());
    options.keys_only = ssh.keys_only || identities_only(params.as_ref());
    options.connect_timeout = if ssh.connect_timeout != DEFAULT_CONNECT_TIMEOUT {
        Duration::from_secs(u64::from(ssh.connect_timeout))
    } else {
        params
            .as_ref()
            .and_then(|params| params.connect_timeout)
            .unwrap_or_else(|| Duration::from_secs(u64::from(ssh.connect_timeout)))
    };
    options.command_timeout = Duration::from_secs(u64::from(ssh.command_timeout));

    let proxy_specs = ssh
        .proxy
        .as_ref()
        .map(|proxy| vec![proxy.clone()])
        .or_else(|| params.as_ref().and_then(|params| params.proxy_jump.clone()))
        .unwrap_or_default();
    options.proxy_jump = proxy_specs
        .iter()
        .map(|spec| jump_options(spec, ssh, config.as_ref(), &options))
        .collect::<anyhow::Result<_>>()?;

    Ok(options)
}

fn configured_keys(server: &NamedServer, ssh: &Ssh) -> Option<Vec<PathBuf>> {
    server
        .keys
        .clone()
        .or_else(|| server.key_path.clone().map(|path| vec![path]))
        .or_else(|| ssh.keys.clone())
        .or_else(|| ssh.key_path.clone().map(|path| vec![path]))
        .map(|paths| paths.into_iter().map(expand_tilde).collect())
}

fn jump_options(
    spec: &str,
    ssh: &Ssh,
    config: Option<&SshConfig>,
    target: &ConnectOptions,
) -> anyhow::Result<ConnectOptions> {
    let jump = parse_jump_spec(spec)?;
    let params = config.map(|config| config.query(&jump.host));
    if proxy_command(params.as_ref()).is_some() {
        anyhow::bail!(
            "ProxyCommand for jump host '{}' is not implemented. Use ProxyJump for the jump host and retry.",
            jump.host
        );
    }

    let user = jump
        .user
        .or_else(|| params.as_ref().and_then(|params| params.user.clone()))
        .unwrap_or_else(|| target.user.clone());
    let mut options = ConnectOptions::new(
        params
            .as_ref()
            .and_then(|params| params.host_name.clone())
            .unwrap_or(jump.host),
        user,
    );
    options.port = jump
        .port
        .or_else(|| params.as_ref().and_then(|params| params.port))
        .unwrap_or(DEFAULT_PORT);
    options.keys = params
        .as_ref()
        .and_then(|params| params.identity_file.clone())
        .unwrap_or_else(|| target.keys.clone());
    options.key_data = target.key_data.clone();
    options.key_passphrase = target.key_passphrase.clone();
    options.keys_only = ssh.keys_only || identities_only(params.as_ref());
    options.connect_timeout = params
        .as_ref()
        .and_then(|params| params.connect_timeout)
        .unwrap_or(target.connect_timeout);
    options.command_timeout = target.command_timeout;
    Ok(options)
}

struct JumpSpec {
    host: String,
    user: Option<String>,
    port: Option<u16>,
}

fn parse_jump_spec(spec: &str) -> anyhow::Result<JumpSpec> {
    let spec = spec.trim();
    if spec.is_empty() {
        anyhow::bail!("ProxyJump cannot be empty. Set `ssh.proxy` to `[user@]host[:port]`.");
    }
    if spec.contains("://") {
        anyhow::bail!(
            "ProxyJump URI '{spec}' is not supported. Use `[user@]host[:port]` and retry."
        );
    }

    let (user, host_port) = spec
        .split_once('@')
        .map_or((None, spec), |(user, rest)| (Some(user.to_string()), rest));
    let (host, port) = match host_port.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => (
            host,
            Some(port.parse::<u16>().with_context(|| {
                format!("ProxyJump '{spec}' has an invalid port. Use `[user@]host[:port]`.")
            })?),
        ),
        _ => (host_port, None),
    };
    if host.is_empty() || user.as_deref() == Some("") {
        anyhow::bail!("ProxyJump '{spec}' is invalid. Use `[user@]host[:port]`.");
    }

    Ok(JumpSpec {
        host: host.to_string(),
        user,
        port,
    })
}

fn load_ssh_config(selection: &SshConfigFiles) -> anyhow::Result<Option<SshConfig>> {
    let (paths, optional) = match selection {
        SshConfigFiles::Enabled(false) => return Ok(None),
        SshConfigFiles::Enabled(true) => {
            let mut paths = Vec::new();
            if let Some(home) = std::env::var_os("HOME") {
                paths.push(PathBuf::from(home).join(".ssh/config"));
            }
            paths.push(PathBuf::from("/etc/ssh/ssh_config"));
            (paths, true)
        }
        SshConfigFiles::Single(path) => (vec![expand_tilde(path)], false),
        SshConfigFiles::Multiple(paths) => (paths.iter().map(expand_tilde).collect(), false),
    };

    let mut config = SshConfig::default();
    for path in paths {
        match File::open(&path) {
            Ok(file) => {
                config = config
                    .parse(
                        &mut BufReader::new(file),
                        ParseRule::ALLOW_UNKNOWN_FIELDS | ParseRule::ALLOW_UNSUPPORTED_FIELDS,
                    )
                    .with_context(|| {
                        format!(
                            "Could not parse SSH config '{}'. Fix the file or disable `ssh.config`.",
                            path.display()
                        )
                    })?;
            }
            Err(error) if optional && error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "Could not read SSH config '{}'. Fix the path or disable `ssh.config`.",
                        path.display()
                    )
                });
            }
        }
    }
    Ok(Some(config))
}

fn reject_proxy_command(name: &str, params: Option<&HostParams>) -> anyhow::Result<()> {
    if proxy_command(params).is_some() {
        anyhow::bail!(
            "Server '{name}' resolves to ProxyCommand, which is not implemented. Configure ProxyJump with `ssh.proxy` or `ProxyJump` in SSH config and retry."
        );
    }
    Ok(())
}

fn proxy_command(params: Option<&HostParams>) -> Option<&str> {
    params?
        .unsupported_fields
        .get("proxycommand")
        .and_then(|values| values.first())
        .map(String::as_str)
}

fn identities_only(params: Option<&HostParams>) -> bool {
    params
        .and_then(|params| params.unsupported_fields.get("identitiesonly"))
        .and_then(|values| values.first())
        .is_some_and(|value| value.eq_ignore_ascii_case("yes"))
}

fn expand_tilde(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    let Some(value) = path.to_str() else {
        return path.to_path_buf();
    };
    if value == "~" {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| path.to_path_buf());
    }
    if let Some(rest) = value.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiji_config::SshConfigFiles;

    fn server(host: &str) -> NamedServer {
        NamedServer {
            host: host.to_string(),
            arch: None,
            user: None,
            port: None,
            key_path: None,
            key_passphrase: None,
            keys: None,
            key_data: None,
        }
    }

    fn ssh(config: SshConfigFiles) -> Ssh {
        Ssh {
            user: None,
            port: 22,
            key_path: None,
            key_passphrase: None,
            connect_timeout: 30,
            command_timeout: 300,
            options: Default::default(),
            proxy: None,
            keys: None,
            key_data: None,
            keys_only: false,
            max_concurrent_starts: 30,
            pool_idle_timeout: 900,
            dns_retries: 3,
            log_level: Default::default(),
            config,
        }
    }

    #[test]
    fn resolves_host_settings_and_proxy_jump_from_ssh_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config");
        std::fs::write(
            &path,
            "Host app\n  HostName 10.0.0.8\n  User deploy\n  Port 2222\n  IdentityFile ~/.ssh/app\n  IdentitiesOnly yes\n  ProxyJump jump\nHost jump\n  HostName 192.0.2.10\n  User bastion\n  Port 2200\n  IdentityFile ~/.ssh/jump\n",
        )
        .expect("write config");

        let options = connect_options(
            "app",
            &server("app"),
            &ssh(SshConfigFiles::Single(path.display().to_string())),
        )
        .expect("resolve");

        assert_eq!(options.host, "10.0.0.8");
        assert_eq!(options.user, "deploy");
        assert_eq!(options.port, 2222);
        assert!(options.keys_only);
        assert_eq!(options.proxy_jump.len(), 1);
        assert_eq!(options.proxy_jump[0].host, "192.0.2.10");
        assert_eq!(options.proxy_jump[0].user, "bastion");
        assert_eq!(options.proxy_jump[0].port, 2200);
    }

    #[test]
    fn jiji_server_values_override_ssh_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config");
        std::fs::write(
            &path,
            "Host app\n  HostName ignored\n  User config-user\n  Port 2200\n",
        )
        .expect("write config");
        let mut named = server("app");
        named.user = Some("server-user".to_string());
        named.port = Some(2022);

        let options = connect_options(
            "app",
            &named,
            &ssh(SshConfigFiles::Single(path.display().to_string())),
        )
        .expect("resolve");

        assert_eq!(options.host, "ignored");
        assert_eq!(options.user, "server-user");
        assert_eq!(options.port, 2022);
    }

    #[test]
    fn rejects_proxy_command_with_an_actionable_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config");
        std::fs::write(
            &path,
            "Host app\n  User deploy\n  ProxyCommand ssh -W %h:%p jump\n",
        )
        .expect("write config");
        let mut ssh = ssh(SshConfigFiles::Single(path.display().to_string()));
        ssh.user = Some("deploy".to_string());

        let error = connect_options("app", &server("app"), &ssh).expect_err("reject");
        assert!(error.to_string().contains("Configure ProxyJump"));
    }
}
