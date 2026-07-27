use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use jiji_config::{Config, NamedServer, RemoteBuilder, Ssh, SshConfigFiles};
use jiji_ssh::{ConnectOptions, SshKey};
use ssh2_config::{HostParams, ParseRule, SshConfig};

const DEFAULT_PORT: u16 = 22;
const DEFAULT_CONNECT_TIMEOUT: u32 = 30;

pub async fn resolve_key_references(
    config: &mut Config,
    loaded: &BTreeMap<String, String>,
    allow_host_env: bool,
) -> anyhow::Result<()> {
    if let Some(ssh) = config.ssh.as_mut() {
        resolve_keys(&mut ssh.keys, "ssh.keys", loaded, allow_host_env).await?;
    }
    for (name, server) in &mut config.servers {
        resolve_keys(
            &mut server.keys,
            &format!("servers.{name}.keys"),
            loaded,
            allow_host_env,
        )
        .await?;
    }
    Ok(())
}

async fn resolve_keys(
    values: &mut Option<Vec<String>>,
    source: &str,
    loaded: &BTreeMap<String, String>,
    allow_host_env: bool,
) -> anyhow::Result<()> {
    if let Some(values) = values {
        for (index, value) in values.iter_mut().enumerate() {
            *value =
                resolve_key_reference(value, &format!("{source}[{index}]"), loaded, allow_host_env)
                    .await?;
        }
    }
    Ok(())
}

async fn resolve_key_reference(
    raw: &str,
    source: &str,
    loaded: &BTreeMap<String, String>,
    allow_host_env: bool,
) -> anyhow::Result<String> {
    if let Some(command) = crate::env_resolution::is_command_expression(raw) {
        let value = crate::env_resolution::resolve_command_value(command)
            .await
            .with_context(|| format!("Could not resolve SSH key path from `{source}`"))?;
        return require_key_path(value, source);
    }
    if !crate::env_resolution::is_bare_all_caps_name(raw) {
        return Ok(raw.to_string());
    }
    let value = crate::env_resolution::resolve_secret_name(raw, loaded, allow_host_env).ok_or_else(|| {
        anyhow::anyhow!(
            "SSH key path variable '{raw}' from `{source}` was not found. Add it to the selected .env file or pass --host-env."
        )
    })?;
    require_key_path(value, source)
}

fn require_key_path(value: String, source: &str) -> anyhow::Result<String> {
    if value.trim().is_empty() {
        anyhow::bail!("SSH key path from `{source}` resolved to an empty value");
    }
    Ok(value)
}

/// Resolves connection options for a `builder.remote` target, reusing `connect_options`'s
/// entire user/port/keys/proxy precedence chain unmodified: the URI's user/port are a
/// server-level override, so they take the same priority `NamedServer.user`/`.port` already
/// do relative to top-level `ssh.*` and `~/.ssh/config`.
pub fn connect_options_for_remote_builder(
    remote: &RemoteBuilder,
    ssh: &Ssh,
) -> anyhow::Result<ConnectOptions> {
    let server = NamedServer {
        host: remote.host.clone(),
        arch: None,
        user: remote.user.clone(),
        port: remote.port,
        key_passphrase: None,
        keys: None,
    };
    connect_options("builder", &server, ssh)
}

pub fn connect_options(
    name: &str,
    server: &NamedServer,
    ssh: &Ssh,
) -> anyhow::Result<ConnectOptions> {
    let config = load_ssh_config(&ssh.config)?;
    let params = config.as_ref().map(|config| config.query(&server.host));

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
                .map(|paths| paths.into_iter().map(SshKey::Path).collect())
        })
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
    options.dns_retries = ssh.dns_retries;
    options.proxy_command = ssh
        .proxy_command
        .clone()
        .or_else(|| proxy_command(params.as_ref()));

    let proxy_specs = ssh
        .proxy
        .as_ref()
        .map(|proxy| vec![proxy.clone()])
        .or_else(|| params.as_ref().and_then(|params| params.proxy_jump.clone()))
        .unwrap_or_default();
    options.proxy_jump = proxy_specs
        .iter()
        .enumerate()
        .map(|(index, spec)| jump_options(spec, ssh, config.as_ref(), &options, index == 0))
        .collect::<anyhow::Result<_>>()?;

    if options.proxy_command.is_some() && !options.proxy_jump.is_empty() {
        anyhow::bail!(
            "Server '{name}' has both a ProxyCommand and a ProxyJump chain configured (via `proxy_command`/`proxy`, or their `~/.ssh/config` equivalents). These are mutually exclusive; configure one or the other and retry."
        );
    }

    Ok(options)
}

fn configured_keys(server: &NamedServer, ssh: &Ssh) -> Option<Vec<SshKey>> {
    server
        .keys
        .clone()
        .or_else(|| ssh.keys.clone())
        .map(|keys| keys.into_iter().map(classify_key).collect())
}

fn classify_key(value: String) -> SshKey {
    if value.trim_start().starts_with("-----BEGIN ") {
        SshKey::Inline(value)
    } else {
        SshKey::Path(expand_tilde(value))
    }
}

/// Builds one jump hop's `ConnectOptions`. `is_first_hop` controls how `ProxyCommand` is handled:
/// only the first hop in a chain is ever reached from the local machine (later hops tunnel
/// through the previous hop's already-established SSH connection), so only the first hop's
/// `ProxyCommand` can ever fire -- matching real OpenSSH. A `ProxyCommand` configured on a later
/// hop would silently never run, so it is rejected explicitly instead.
fn jump_options(
    spec: &str,
    ssh: &Ssh,
    config: Option<&SshConfig>,
    target: &ConnectOptions,
    is_first_hop: bool,
) -> anyhow::Result<ConnectOptions> {
    let jump = parse_jump_spec(spec)?;
    let params = config.map(|config| config.query(&jump.host));
    if !is_first_hop && proxy_command(params.as_ref()).is_some() {
        anyhow::bail!(
            "ProxyCommand on jump host '{}' is not supported: only the first hop in a ProxyJump chain can use ProxyCommand. Reorder the chain so it is first, or remove it, and retry.",
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
        .map(|paths| paths.into_iter().map(SshKey::Path).collect())
        .unwrap_or_else(|| target.keys.clone());
    options.key_passphrase = target.key_passphrase.clone();
    options.keys_only = ssh.keys_only || identities_only(params.as_ref());
    options.connect_timeout = params
        .as_ref()
        .and_then(|params| params.connect_timeout)
        .unwrap_or(target.connect_timeout);
    options.command_timeout = target.command_timeout;
    options.dns_retries = target.dns_retries;
    if is_first_hop {
        options.proxy_command = proxy_command(params.as_ref());
    }
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

/// `ssh2_config` tokenizes every unrecognized directive's value on whitespace (it doesn't know
/// `ProxyCommand`'s value is a single shell command), so the tokens are rejoined with spaces here.
/// This loses fidelity for a value that quoted an argument containing spaces, which matches the
/// project's existing "supported OpenSSH config fields" framing rather than full compatibility.
fn proxy_command(params: Option<&HostParams>) -> Option<String> {
    params?
        .unsupported_fields
        .get("proxycommand")
        .filter(|values| !values.is_empty())
        .map(|values| values.join(" "))
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
            key_passphrase: None,
            keys: None,
        }
    }

    fn ssh(config: SshConfigFiles) -> Ssh {
        Ssh {
            user: None,
            port: 22,
            key_passphrase: None,
            connect_timeout: 30,
            command_timeout: 300,
            options: Default::default(),
            proxy: None,
            proxy_command: None,
            keys: None,
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
    fn resolves_proxy_command_from_ssh_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config");
        std::fs::write(
            &path,
            "Host app\n  User deploy\n  ProxyCommand ssh -W %h:%p jump\n",
        )
        .expect("write config");
        let mut ssh = ssh(SshConfigFiles::Single(path.display().to_string()));
        ssh.user = Some("deploy".to_string());

        let options = connect_options("app", &server("app"), &ssh).expect("resolve");
        assert_eq!(options.proxy_command.as_deref(), Some("ssh -W %h:%p jump"));
    }

    #[test]
    fn jiji_proxy_command_overrides_ssh_config_proxy_command() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config");
        std::fs::write(
            &path,
            "Host app\n  User deploy\n  ProxyCommand from-config\n",
        )
        .expect("write config");
        let mut ssh = ssh(SshConfigFiles::Single(path.display().to_string()));
        ssh.user = Some("deploy".to_string());
        ssh.proxy_command = Some("from-jiji-yml".to_string());

        let options = connect_options("app", &server("app"), &ssh).expect("resolve");
        assert_eq!(options.proxy_command.as_deref(), Some("from-jiji-yml"));
    }

    #[test]
    fn proxy_command_and_proxy_jump_together_are_rejected() {
        let mut ssh = ssh(SshConfigFiles::Enabled(false));
        ssh.user = Some("deploy".to_string());
        ssh.proxy_command = Some("nc %h %p".to_string());
        ssh.proxy = Some("bastion.example.com".to_string());

        let error = connect_options("app", &server("app"), &ssh).expect_err("reject");
        assert!(error.to_string().contains("mutually exclusive"));
    }

    #[test]
    fn remote_builder_uri_user_and_port_override_ssh_defaults() {
        let mut top_level_ssh = ssh(SshConfigFiles::Enabled(false));
        top_level_ssh.user = Some("default-user".to_string());

        let remote = RemoteBuilder {
            host: "builder.example.com".to_string(),
            user: Some("builder-user".to_string()),
            port: Some(2222),
        };
        let options = connect_options_for_remote_builder(&remote, &top_level_ssh).expect("resolve");
        assert_eq!(options.host, "builder.example.com");
        assert_eq!(options.user, "builder-user");
        assert_eq!(options.port, 2222);
    }

    #[test]
    fn remote_builder_uri_falls_through_to_ssh_defaults_when_omitted() {
        let mut top_level_ssh = ssh(SshConfigFiles::Enabled(false));
        top_level_ssh.user = Some("default-user".to_string());

        let remote = RemoteBuilder {
            host: "builder.example.com".to_string(),
            user: None,
            port: None,
        };
        let options = connect_options_for_remote_builder(&remote, &top_level_ssh).expect("resolve");
        assert_eq!(options.user, "default-user");
        assert_eq!(options.port, 22);
    }

    #[test]
    fn remote_builder_uri_falls_through_to_ssh_config_when_ssh_yml_omits_user() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config");
        std::fs::write(
            &path,
            "Host builder.example.com\n  User config-user\n  Port 2200\n",
        )
        .expect("write config");

        let remote = RemoteBuilder {
            host: "builder.example.com".to_string(),
            user: None,
            port: None,
        };
        let options = connect_options_for_remote_builder(
            &remote,
            &ssh(SshConfigFiles::Single(path.display().to_string())),
        )
        .expect("resolve");
        assert_eq!(options.user, "config-user");
        assert_eq!(options.port, 2200);
    }

    #[test]
    fn remote_builder_uri_without_any_user_is_a_clear_error() {
        let error = connect_options_for_remote_builder(
            &RemoteBuilder {
                host: "builder.example.com".to_string(),
                user: None,
                port: None,
            },
            &ssh(SshConfigFiles::Enabled(false)),
        )
        .expect_err("reject");
        assert!(error.to_string().contains("no SSH user configured"));
    }

    #[test]
    fn proxy_command_on_a_later_jump_hop_is_rejected() {
        // Exercises `jump_options` directly with `is_first_hop: false` -- a real multi-hop
        // ProxyJump chain needing ssh-config's comma-separated `ProxyJump` parsing is exercised
        // end-to-end elsewhere; this test only needs to prove the `is_first_hop` gate itself.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config");
        std::fs::write(&path, "Host second\n  ProxyCommand nc %h %p\n").expect("write config");
        let mut file = File::open(&path).expect("open config");
        let config = SshConfig::default()
            .parse(
                &mut BufReader::new(&mut file),
                ParseRule::ALLOW_UNKNOWN_FIELDS | ParseRule::ALLOW_UNSUPPORTED_FIELDS,
            )
            .expect("parse config");
        let ssh = ssh(SshConfigFiles::Enabled(false));
        let target = ConnectOptions::new("target.example.com", "deploy");

        let error = jump_options("second", &ssh, Some(&config), &target, false)
            .expect_err("reject a non-first hop's ProxyCommand");
        assert!(error.to_string().contains("only the first hop"));
    }

    #[tokio::test]
    async fn key_references_resolve_literals_env_values_and_commands() {
        let mut config: Config = serde_yaml::from_str(
            r#"
project: demo
builder: { engine: podman }
ssh:
  user: deploy
  keys:
    - ~/.ssh/literal
    - APP_KEY_PATH
    - "$(printf /tmp/command-key)"
servers:
  app:
    host: app.example.com
    keys: [SERVER_KEY_PATH]
services: {}
"#,
        )
        .expect("parse config");
        let loaded = BTreeMap::from([
            ("APP_KEY_PATH".to_string(), "/tmp/app-key".to_string()),
            ("SERVER_KEY_PATH".to_string(), "/tmp/server-key".to_string()),
        ]);

        resolve_key_references(&mut config, &loaded, false)
            .await
            .expect("resolve");

        assert_eq!(
            config.ssh.unwrap().keys.unwrap(),
            ["~/.ssh/literal", "/tmp/app-key", "/tmp/command-key"]
        );
        assert_eq!(
            config.servers["app"].keys.as_deref(),
            Some(["/tmp/server-key".to_string()].as_slice())
        );
    }

    #[tokio::test]
    async fn missing_key_environment_reference_is_actionable() {
        let mut config: Config = serde_yaml::from_str(
            r#"
project: demo
builder: { engine: podman }
ssh:
  user: deploy
  keys: [MISSING_KEY_PATH]
servers:
  app:
    host: app.example.com
services: {}
"#,
        )
        .expect("parse config");

        let error = resolve_key_references(&mut config, &BTreeMap::new(), false)
            .await
            .expect_err("missing reference");
        assert!(error.to_string().contains("MISSING_KEY_PATH"));
        assert!(error.to_string().contains("--host-env"));
    }

    #[test]
    fn resolved_keys_distinguish_paths_from_inline_material() {
        assert!(matches!(
            classify_key("/tmp/deploy-key".to_string()),
            SshKey::Path(path) if path == Path::new("/tmp/deploy-key")
        ));
        assert!(matches!(
            classify_key(
                "  -----BEGIN OPENSSH PRIVATE KEY-----\nvalue\n-----END OPENSSH PRIVATE KEY-----"
                    .to_string()
            ),
            SshKey::Inline(_)
        ));
    }
}
