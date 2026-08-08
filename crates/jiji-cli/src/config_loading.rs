use std::path::{Path, PathBuf};

use jiji_config::Config;

use crate::{env_resolution, ssh_adapter};

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
    ssh_adapter::resolve_key_references(&mut config, &loaded, crate::ssh_host_env_enabled())
        .await?;
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
}
