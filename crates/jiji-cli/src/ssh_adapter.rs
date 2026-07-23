use std::path::PathBuf;
use std::time::Duration;

use jiji_config::{NamedServer, Ssh};
use jiji_ssh::ConnectOptions;

/// Builds SSH connect options for a named server, layering its per-server overrides on top of
/// the global `ssh:` block. `keys_only`/`connect_timeout`/`command_timeout` have no per-server
/// override in the config schema, so they always come from the global block.
pub fn connect_options(
    name: &str,
    server: &NamedServer,
    ssh: &Ssh,
) -> anyhow::Result<ConnectOptions> {
    let user = server
        .user
        .clone()
        .or_else(|| ssh.user.clone())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Server '{name}' has no SSH user configured. Set `user:` on the server or add a top-level `ssh.user:` in your deploy config."
            )
        })?;

    let port = server.port.unwrap_or(ssh.port);

    let keys: Vec<PathBuf> = server
        .keys
        .clone()
        .or_else(|| server.key_path.clone().map(|p| vec![p]))
        .or_else(|| ssh.keys.clone())
        .or_else(|| ssh.key_path.clone().map(|p| vec![p]))
        .unwrap_or_default()
        .into_iter()
        .map(PathBuf::from)
        .collect();

    let key_data = server
        .key_data
        .clone()
        .or_else(|| ssh.key_data.clone())
        .unwrap_or_default();

    let key_passphrase = server
        .key_passphrase
        .clone()
        .or_else(|| ssh.key_passphrase.clone());

    let mut options = ConnectOptions::new(server.host.clone(), user);
    options.port = port;
    options.keys = keys;
    options.key_data = key_data;
    options.key_passphrase = key_passphrase;
    options.keys_only = ssh.keys_only;
    options.connect_timeout = Duration::from_secs(ssh.connect_timeout as u64);
    options.command_timeout = Duration::from_secs(ssh.command_timeout as u64);

    Ok(options)
}
