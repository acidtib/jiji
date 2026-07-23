use std::collections::BTreeMap;

use anyhow::Context;
use jiji_config::{ContainerEngine, Registry};
use jiji_ssh::SshSession;

use crate::{env_resolution, local_exec};

const NAMESPACED_HOSTS: &[&str] = &[
    "ghcr.io",
    "docker.io",
    "registry-1.docker.io",
    "index.docker.io",
];

pub fn full_image_name(
    registry: &Registry,
    project: &str,
    service: &str,
    tag: &str,
) -> anyhow::Result<String> {
    let server = registry.server.as_deref().ok_or_else(|| {
        anyhow::anyhow!("Remote registry has no `server:` configured. Set builder.registry.server.")
    })?;
    let namespace = if NAMESPACED_HOSTS.contains(&server) {
        Some(registry.username.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "Registry '{server}' requires `builder.registry.username` for image names."
            )
        })?)
    } else {
        None
    };
    Ok(match namespace {
        Some(namespace) => format!("{server}/{namespace}/{project}-{service}:{tag}"),
        None => format!("{server}/{project}-{service}:{tag}"),
    })
}

pub fn render_login_command(engine: ContainerEngine, server: &str, username: &str) -> String {
    format!("{engine} login {server} --username {username} --password-stdin")
}

pub fn render_login_args(server: &str, username: &str) -> Vec<String> {
    vec![
        "login".into(),
        server.into(),
        "--username".into(),
        username.into(),
        "--password-stdin".into(),
    ]
}

pub fn resolve_registry_password(
    raw: &str,
    loaded: &BTreeMap<String, String>,
    allow_host_env: bool,
) -> anyhow::Result<String> {
    if !env_resolution::is_bare_all_caps_name(raw) {
        return Ok(raw.to_string());
    }
    env_resolution::resolve_secret_name(raw, loaded, allow_host_env).ok_or_else(|| {
        anyhow::anyhow!(
            "Registry password variable '{raw}' was not found. Add it to the selected .env file, pass --host-env, or configure a literal password."
        )
    })
}

pub async fn login_local(
    engine: ContainerEngine,
    registry: &Registry,
    password: &str,
) -> anyhow::Result<()> {
    let server = registry
        .server
        .as_deref()
        .context("Remote registry has no `server:` configured. Set builder.registry.server.")?;
    let username = registry.username.as_deref().context(
        "Registry login requires `builder.registry.username`. Configure it or use a public registry.",
    )?;
    let result = local_exec::run_captured(
        &engine.to_string(),
        &render_login_args(server, username),
        Some(password.as_bytes()),
        None,
    )
    .await?;
    if !result.success {
        anyhow::bail!(
            "Registry login to '{server}' failed (exit {:?}): {}. Verify the registry credentials and retry.",
            result.code,
            result.stderr.trim()
        );
    }
    Ok(())
}

pub async fn login_remote(
    session: &SshSession,
    engine: ContainerEngine,
    registry: &Registry,
    password: &str,
) -> anyhow::Result<()> {
    let server = registry
        .server
        .as_deref()
        .context("Remote registry has no `server:` configured. Set builder.registry.server.")?;
    let username = registry.username.as_deref().context(
        "Registry login requires `builder.registry.username`. Configure it or use a public registry.",
    )?;
    let result = session
        .execute_with_input(
            &render_login_command(engine, server, username),
            password.as_bytes(),
        )
        .await?;
    if !result.success {
        anyhow::bail!(
            "Registry login to '{server}' failed on a deploy host: {}. Verify the credentials and retry.",
            result.stderr.trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiji_config::RegistryType;

    fn registry(server: &str, username: Option<&str>) -> Registry {
        Registry {
            kind: RegistryType::Remote,
            port: 443,
            server: Some(server.into()),
            username: username.map(str::to_string),
            password: None,
        }
    }

    #[test]
    fn namespaced_registries_include_the_username() {
        assert_eq!(
            full_image_name(&registry("ghcr.io", Some("alice")), "demo", "web", "v1").unwrap(),
            "ghcr.io/alice/demo-web:v1"
        );
        assert_eq!(
            full_image_name(&registry("registry.example.com", None), "demo", "web", "v1").unwrap(),
            "registry.example.com/demo-web:v1"
        );
    }

    #[test]
    fn login_rendering_uses_stdin_and_real_argv() {
        assert_eq!(
            render_login_command(ContainerEngine::Docker, "ghcr.io", "alice"),
            "docker login ghcr.io --username alice --password-stdin"
        );
        assert_eq!(
            render_login_args("ghcr.io", "alice"),
            [
                "login",
                "ghcr.io",
                "--username",
                "alice",
                "--password-stdin"
            ]
        );
    }

    #[test]
    fn password_resolution_distinguishes_names_from_literals() {
        let loaded = BTreeMap::from([("TOKEN".into(), "secret".into())]);
        assert_eq!(
            resolve_registry_password("TOKEN", &loaded, false).unwrap(),
            "secret"
        );
        assert_eq!(
            resolve_registry_password("literal-value", &loaded, false).unwrap(),
            "literal-value"
        );
        assert!(resolve_registry_password("MISSING", &loaded, false).is_err());
    }
}
