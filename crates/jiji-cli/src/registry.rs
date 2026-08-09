use std::collections::BTreeMap;

use anyhow::Context;
use jiji_config::{ContainerEngine, Registry};
use jiji_ssh::SshSession;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{sleep, Duration};

use crate::{env_resolution, local_exec};

const NAMESPACED_HOSTS: &[&str] = &[
    "ghcr.io",
    "docker.io",
    "registry-1.docker.io",
    "index.docker.io",
];
pub const LOCAL_REGISTRY_NAME: &str = "jiji-registry";
pub const LOCAL_REGISTRY_IMAGE: &str = "registry:2";

/// The tag-less repository prefix `full_image_name` appends a tag to -- exposed on its own so
/// `service prune` can filter an `images` listing by repository without a tag, or knowing one.
pub fn repo_reference(registry: &Registry, project: &str, service: &str) -> anyhow::Result<String> {
    if registry.is_local() {
        return Ok(format!("localhost:{}/{project}-{service}", registry.port));
    }
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
        Some(namespace) => format!("{server}/{namespace}/{project}-{service}"),
        None => format!("{server}/{project}-{service}"),
    })
}

pub fn full_image_name(
    registry: &Registry,
    project: &str,
    service: &str,
    tag: &str,
) -> anyhow::Result<String> {
    Ok(format!(
        "{}:{tag}",
        repo_reference(registry, project, service)?
    ))
}

pub fn render_local_registry_inspect() -> Vec<String> {
    vec![
        "container".into(),
        "inspect".into(),
        "--format".into(),
        r#"{{index .Config.Labels "jiji.managed"}}|{{index .Config.Labels "jiji.resource"}}|{{index .Config.Labels "jiji.registry.port"}}|{{.State.Running}}"#.into(),
        LOCAL_REGISTRY_NAME.into(),
    ]
}

pub fn render_local_registry_run(port: u16) -> Vec<String> {
    vec![
        "run".into(),
        "-d".into(),
        "--restart".into(),
        "unless-stopped".into(),
        "--name".into(),
        LOCAL_REGISTRY_NAME.into(),
        "--label".into(),
        "jiji.managed=true".into(),
        "--label".into(),
        "jiji.resource=registry".into(),
        "--label".into(),
        format!("jiji.registry.port={port}"),
        "-p".into(),
        format!("127.0.0.1:{port}:5000"),
        LOCAL_REGISTRY_IMAGE.into(),
    ]
}

pub fn render_local_registry_start() -> Vec<String> {
    vec!["start".into(), LOCAL_REGISTRY_NAME.into()]
}

pub fn render_local_registry_remove() -> Vec<String> {
    vec![
        "container".into(),
        "rm".into(),
        "-f".into(),
        LOCAL_REGISTRY_NAME.into(),
    ]
}

pub async fn ensure_local_registry(
    engine: ContainerEngine,
    registry: &Registry,
) -> anyhow::Result<()> {
    if !registry.is_local() {
        return Ok(());
    }
    if !local_exec::command_exists(&engine.to_string()).await {
        anyhow::bail!(
            "Container engine '{engine}' was not found locally. Install it or update builder.engine."
        );
    }

    match local_registry_state(engine, registry.port).await? {
        Some(false) => {
            run_local_registry_command(
                engine,
                render_local_registry_start(),
                "start the local registry",
            )
            .await?;
        }
        Some(true) => {}
        None => {
            run_local_registry_command(
                engine,
                render_local_registry_run(registry.port),
                "create the local registry",
            )
            .await?;
        }
    }

    wait_for_registry(registry.port).await
}

pub async fn local_registry_state(
    engine: ContainerEngine,
    expected_port: u16,
) -> anyhow::Result<Option<bool>> {
    let inspect = local_exec::run_captured(
        &engine.to_string(),
        &render_local_registry_inspect(),
        None,
        None,
    )
    .await?;
    if inspect.success {
        return parse_local_registry_running(&inspect.stdout, expected_port).map(Some);
    }
    let stderr = inspect.stderr.to_ascii_lowercase();
    if stderr.contains("no such container")
        || stderr.contains("no container with name or id")
        || stderr.contains("does not exist")
    {
        return Ok(None);
    }
    anyhow::bail!(
        "Could not inspect local registry container '{LOCAL_REGISTRY_NAME}' with {engine} (exit {:?}): {}. Restore local engine access and retry.",
        inspect.code,
        inspect.stderr.trim()
    )
}

pub async fn remove_local_registry(
    engine: ContainerEngine,
    expected_port: u16,
) -> anyhow::Result<()> {
    if local_registry_state(engine, expected_port).await?.is_none() {
        return Ok(());
    }
    run_local_registry_command(
        engine,
        render_local_registry_remove(),
        "remove the local registry",
    )
    .await
}

fn parse_local_registry_running(output: &str, expected_port: u16) -> anyhow::Result<bool> {
    let fields: Vec<&str> = output.trim().split('|').collect();
    if fields.len() != 4
        || fields[0] != "true"
        || fields[1] != "registry"
        || fields[2].parse::<u16>() != Ok(expected_port)
    {
        anyhow::bail!(
            "Container '{LOCAL_REGISTRY_NAME}' already exists but is not Jiji's registry on port {expected_port}. Rename or remove that container, then retry."
        );
    }
    match fields[3] {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => anyhow::bail!(
            "Could not determine whether local registry container '{LOCAL_REGISTRY_NAME}' is running. Inspect or remove that container, then retry."
        ),
    }
}

async fn run_local_registry_command(
    engine: ContainerEngine,
    args: Vec<String>,
    action: &str,
) -> anyhow::Result<()> {
    let result = local_exec::run_captured(&engine.to_string(), &args, None, None).await?;
    if !result.success {
        anyhow::bail!(
            "Could not {action} with {engine} (exit {:?}): {}. Fix the local container or port conflict and retry.",
            result.code,
            result.stderr.trim()
        );
    }
    Ok(())
}

async fn wait_for_registry(port: u16) -> anyhow::Result<()> {
    for _ in 0..30 {
        if registry_responds(port).await {
            return Ok(());
        }
        sleep(Duration::from_millis(100)).await;
    }
    anyhow::bail!(
        "Local registry did not become ready at http://127.0.0.1:{port}/v2/. Inspect container '{LOCAL_REGISTRY_NAME}', fix it, and retry."
    )
}

async fn registry_responds(port: u16) -> bool {
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)).await else {
        return false;
    };
    if stream
        .write_all(b"GET /v2/ HTTP/1.0\r\nHost: localhost\r\n\r\n")
        .await
        .is_err()
    {
        return false;
    }
    let mut response = [0_u8; 64];
    let Ok(read) = stream.read(&mut response).await else {
        return false;
    };
    response[..read].starts_with(b"HTTP/1.0 200") || response[..read].starts_with(b"HTTP/1.1 200")
}

/// Quotes a config-derived value for safe interpolation into a remote shell command string.
/// POSIX single-quoting: wrap in `'...'`, and turn any embedded `'` into `'\''` (close the
/// quote, emit an escaped literal quote, reopen the quote). Safe for any byte sequence.
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Registry server and username, resolved together since login requires both.
pub struct LoginCredentials<'a> {
    pub server: &'a str,
    pub username: &'a str,
}

pub fn require_server(registry: &Registry) -> anyhow::Result<&str> {
    registry
        .server
        .as_deref()
        .context("Remote registry has no `server:` configured. Set builder.registry.server.")
}

pub fn require_login_credentials(registry: &Registry) -> anyhow::Result<LoginCredentials<'_>> {
    let server = require_server(registry)?;
    let username = registry.username.as_deref().context(
        "Registry login requires `builder.registry.username`. Configure it or use a public registry.",
    )?;
    Ok(LoginCredentials { server, username })
}

pub fn render_login_command(engine: ContainerEngine, server: &str, username: &str) -> String {
    format!(
        "{engine} login {} --username {} --password-stdin",
        shell_quote(server),
        shell_quote(username)
    )
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

pub fn render_logout_command(engine: ContainerEngine, server: &str) -> String {
    format!("{engine} logout {}", shell_quote(server))
}

pub fn render_logout_args(server: &str) -> Vec<String> {
    vec!["logout".into(), server.into()]
}

pub async fn resolve_registry_password(
    raw: &str,
    loaded: &BTreeMap<String, String>,
    allow_host_env: bool,
) -> anyhow::Result<String> {
    if let Some(command) = env_resolution::is_command_expression(raw) {
        return env_resolution::resolve_command_value(command).await;
    }
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
    let credentials = require_login_credentials(registry)?;
    let result = local_exec::run_captured(
        &engine.to_string(),
        &render_login_args(credentials.server, credentials.username),
        Some(password.as_bytes()),
        None,
    )
    .await?;
    if !result.success {
        anyhow::bail!(
            "Registry login to '{}' failed (exit {:?}): {}. Verify the registry credentials and retry.",
            credentials.server,
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
    let credentials = require_login_credentials(registry)?;
    let result = session
        .execute_with_input(
            &render_login_command(engine, credentials.server, credentials.username),
            password.as_bytes(),
        )
        .await?;
    if !result.success {
        anyhow::bail!(
            "Registry login to '{}' failed on a deploy host: {}. Verify the credentials and retry.",
            credentials.server,
            result.stderr.trim()
        );
    }
    Ok(())
}

/// Outcome of a logout attempt, distinguishing a genuine logout from an engine reporting the
/// target was already logged out (both are success from the caller's point of view).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthOutcome {
    LoggedOut,
    AlreadyLoggedOut,
}

/// Classifies a logout command's result. Docker's `logout` always exits 0. Podman's `logout`
/// exits nonzero with a "not logged in" style message when there were no stored credentials --
/// that case is idempotent success, not a failure.
fn classify_logout(success: bool, stderr: &str) -> Option<AuthOutcome> {
    if success {
        return Some(AuthOutcome::LoggedOut);
    }
    if stderr.to_ascii_lowercase().contains("not logged in") {
        return Some(AuthOutcome::AlreadyLoggedOut);
    }
    None
}

pub async fn logout_local(
    engine: ContainerEngine,
    registry: &Registry,
) -> anyhow::Result<AuthOutcome> {
    let server = require_server(registry)?;
    let result =
        local_exec::run_captured(&engine.to_string(), &render_logout_args(server), None, None)
            .await?;
    classify_logout(result.success, &result.stderr).ok_or_else(|| {
        anyhow::anyhow!(
            "Registry logout from '{server}' failed (exit {:?}): {}. Verify local engine access and retry.",
            result.code,
            result.stderr.trim()
        )
    })
}

pub async fn logout_remote(
    session: &SshSession,
    engine: ContainerEngine,
    registry: &Registry,
) -> anyhow::Result<AuthOutcome> {
    let server = require_server(registry)?;
    let result = session
        .execute(&render_logout_command(engine, server))
        .await?;
    classify_logout(result.success, &result.stderr).ok_or_else(|| {
        anyhow::anyhow!(
            "Registry logout from '{server}' failed on a deploy host: {}. Verify engine access and retry.",
            result.stderr.trim()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn registry(server: &str, username: Option<&str>) -> Registry {
        Registry {
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
    fn local_registry_names_and_lifecycle_commands_are_loopback_only() {
        let local = Registry {
            port: 31270,
            server: None,
            username: None,
            password: None,
        };
        assert_eq!(
            full_image_name(&local, "demo", "web", "v1").unwrap(),
            "localhost:31270/demo-web:v1"
        );
        let run = render_local_registry_run(31270);
        assert!(run.contains(&"127.0.0.1:31270:5000".to_string()));
        assert!(run.contains(&"jiji.resource=registry".to_string()));
        assert!(!run.iter().any(|arg| arg == "0.0.0.0:31270:5000"));
        assert!(parse_local_registry_running("true|registry|31270|true\n", 31270).unwrap());
        assert!(!parse_local_registry_running("true|registry|31270|false\n", 31270).unwrap());
        assert!(parse_local_registry_running("false|registry|31270|true\n", 31270).is_err());
        assert!(parse_local_registry_running("true|registry|5000|true\n", 31270).is_err());
        assert_eq!(
            render_local_registry_remove(),
            ["container", "rm", "-f", "jiji-registry"]
        );
    }

    #[test]
    fn login_rendering_uses_stdin_and_real_argv() {
        assert_eq!(
            render_login_command(ContainerEngine::Docker, "ghcr.io", "alice"),
            "docker login 'ghcr.io' --username 'alice' --password-stdin"
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
    fn logout_rendering_uses_real_argv_and_quoted_remote_command() {
        assert_eq!(
            render_logout_command(ContainerEngine::Podman, "ghcr.io"),
            "podman logout 'ghcr.io'"
        );
        assert_eq!(render_logout_args("ghcr.io"), ["logout", "ghcr.io"]);
    }

    #[test]
    fn shell_quote_neutralizes_hostile_registry_values() {
        assert_eq!(shell_quote("ghcr.io"), "'ghcr.io'");
        assert_eq!(
            shell_quote("evil'; rm -rf / #"),
            r#"'evil'\''; rm -rf / #'"#
        );
        assert_eq!(
            render_login_command(ContainerEngine::Docker, "host'; touch pwned #", "u'ser"),
            "docker login 'host'\\''; touch pwned #' --username 'u'\\''ser' --password-stdin"
        );
    }

    #[test]
    fn logout_classification_treats_not_logged_in_as_idempotent_success() {
        assert_eq!(classify_logout(true, ""), Some(AuthOutcome::LoggedOut));
        assert_eq!(
            classify_logout(false, "Error: not logged into ghcr.io\n"),
            Some(AuthOutcome::AlreadyLoggedOut)
        );
        assert_eq!(classify_logout(false, "permission denied"), None);
    }

    #[tokio::test]
    async fn password_resolution_distinguishes_names_from_literals() {
        let loaded = BTreeMap::from([("TOKEN".into(), "secret".into())]);
        assert_eq!(
            resolve_registry_password("TOKEN", &loaded, false)
                .await
                .unwrap(),
            "secret"
        );
        assert_eq!(
            resolve_registry_password("literal-value", &loaded, false)
                .await
                .unwrap(),
            "literal-value"
        );
        assert!(resolve_registry_password("MISSING", &loaded, false)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn password_resolution_runs_command_expressions() {
        let loaded = BTreeMap::new();
        assert_eq!(
            resolve_registry_password("$(echo -n secret-token)", &loaded, false)
                .await
                .unwrap(),
            "secret-token"
        );
        assert!(resolve_registry_password("$(exit 1)", &loaded, false)
            .await
            .is_err());
    }

    #[test]
    fn missing_credentials_produce_actionable_errors() {
        let no_server = Registry {
            port: 443,
            server: None,
            username: None,
            password: None,
        };
        assert!(require_server(&no_server).is_err());
        assert!(require_login_credentials(&no_server).is_err());

        let no_username = registry("ghcr.io", None);
        assert!(require_server(&no_username).is_ok());
        assert!(require_login_credentials(&no_username).is_err());
    }
}
