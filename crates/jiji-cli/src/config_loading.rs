use std::path::{Path, PathBuf};

use jiji_config::Config;

use crate::{env_resolution, ssh_adapter};

fn resolve_server_host_references(
    config: &mut Config,
    loaded: &std::collections::BTreeMap<String, String>,
    allow_host_env: bool,
) -> anyhow::Result<()> {
    for (name, server) in &mut config.servers {
        if !env_resolution::is_bare_all_caps_name(&server.host) {
            continue;
        }
        let reference = server.host.clone();
        let host = env_resolution::resolve_secret_name(&reference, loaded, allow_host_env)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Server host variable '{reference}' for `servers.{name}.host` was not found. Add it to the selected .env file or pass --host-env."
                )
            })?;
        if host.trim().is_empty() {
            anyhow::bail!("Server host variable '{reference}' for `servers.{name}.host` is empty");
        }
        server.host = host;
    }
    Ok(())
}

pub async fn load_config_for_ssh(
    environment: Option<&str>,
    explicit_path: Option<&Path>,
    start: &Path,
) -> anyhow::Result<(Config, PathBuf)> {
    let (mut config, path) = jiji_config::load_config(environment, explicit_path, start)?;
    if !jiji_config::validate_config(&config).valid {
        return Ok((config, path));
    }
    let project_root = env_resolution::project_root_from_config_path(&path);
    let (loaded, _) =
        env_resolution::load_env_file(&project_root, environment, config.secrets_path.as_deref())?;
    let allow_host_env = crate::ssh_host_env_enabled();
    resolve_server_host_references(&mut config, &loaded, allow_host_env)?;
    ssh_adapter::resolve_key_references(&mut config, &loaded, allow_host_env).await?;
    Ok((config, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resolves_ssh_key_paths_from_the_project_env_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_dir = dir.path().join(".jiji");
        std::fs::create_dir(&config_dir).expect("create config dir");
        let config_path = config_dir.join("deploy.yml");
        std::fs::write(
            &config_path,
            r#"
project: demo
builder: { engine: podman }
servers:
  app: { host: app.example.com }
services: {}
ssh:
  user: deploy
  keys: [SSH_KEY_PATH]
"#,
        )
        .expect("write config");
        std::fs::write(dir.path().join(".env"), "SSH_KEY_PATH=/tmp/deploy-key\n")
            .expect("write env");

        let (config, _) = load_config_for_ssh(None, Some(&config_path), dir.path())
            .await
            .expect("load");

        assert_eq!(
            config.ssh.unwrap().keys.unwrap(),
            ["/tmp/deploy-key".to_string()]
        );
    }

    #[tokio::test]
    async fn resolves_server_hosts_from_the_project_env_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_dir = dir.path().join(".jiji");
        std::fs::create_dir(&config_dir).expect("create config dir");
        let config_path = config_dir.join("deploy.production.yml");
        std::fs::write(
            &config_path,
            r#"
project: demo
builder: { engine: podman }
servers:
  app: { host: SERVER_IP }
services: {}
ssh: { user: root }
"#,
        )
        .expect("write config");
        std::fs::write(dir.path().join(".env.production"), "SERVER_IP=192.0.2.10\n")
            .expect("write env");

        let (config, _) = load_config_for_ssh(Some("production"), Some(&config_path), dir.path())
            .await
            .expect("load");

        assert_eq!(config.servers["app"].host, "192.0.2.10");
    }

    #[tokio::test]
    async fn missing_server_host_reference_is_actionable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_dir = dir.path().join(".jiji");
        std::fs::create_dir(&config_dir).expect("create config dir");
        let config_path = config_dir.join("deploy.yml");
        std::fs::write(
            &config_path,
            r#"
project: demo
builder: { engine: podman }
servers:
  app: { host: SERVER_IP }
services: {}
ssh: { user: root }
"#,
        )
        .expect("write config");

        let error = load_config_for_ssh(None, Some(&config_path), dir.path())
            .await
            .expect_err("missing host variable must fail");

        assert!(error
            .to_string()
            .contains("Server host variable 'SERVER_IP' for `servers.app.host` was not found"));
    }
}
