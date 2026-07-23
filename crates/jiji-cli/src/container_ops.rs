use jiji_config::ContainerEngine;
use jiji_network::NetworkedContainerRun;
use jiji_ssh::{CommandResult, SshSession};

pub async fn image_exists(
    session: &SshSession,
    engine: ContainerEngine,
    image: &str,
) -> anyhow::Result<bool> {
    let result = session
        .execute(&format!("{engine} image inspect {image} >/dev/null 2>&1"))
        .await?;
    Ok(result.success)
}

pub async fn ensure_image(
    session: &SshSession,
    engine: ContainerEngine,
    image: &str,
) -> anyhow::Result<()> {
    if image_exists(session, engine, image).await? {
        return Ok(());
    }
    let command = format!("{engine} pull {image}");
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)
}

/// `None` means "no such container" (not an error); any other failure to inspect propagates.
pub async fn inspect_status(
    session: &SshSession,
    engine: ContainerEngine,
    name: &str,
) -> anyhow::Result<Option<String>> {
    let command = format!("{engine} inspect {name} --format '{{{{.State.Status}}}}'");
    let result = session.execute(&command).await?;
    if !result.success {
        return Ok(None);
    }
    Ok(Some(result.stdout.trim().to_string()))
}

pub async fn create_and_start(
    session: &SshSession,
    run: &NetworkedContainerRun,
) -> anyhow::Result<()> {
    let command = run.shell_command();
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)
}

pub async fn stop(session: &SshSession, engine: ContainerEngine, name: &str) -> anyhow::Result<()> {
    let command = format!("{engine} stop {name}");
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)
}

pub async fn remove(
    session: &SshSession,
    engine: ContainerEngine,
    name: &str,
) -> anyhow::Result<()> {
    let command = format!("{engine} rm -f {name}");
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)
}

pub async fn logs_tail(
    session: &SshSession,
    engine: ContainerEngine,
    name: &str,
    lines: u32,
) -> anyhow::Result<String> {
    let command = format!("{engine} logs --tail {lines} {name} 2>&1");
    let result = session.execute(&command).await?;
    Ok(result.stdout)
}

fn ensure_success(
    session: &SshSession,
    command: &str,
    result: &CommandResult,
) -> anyhow::Result<()> {
    if result.success {
        return Ok(());
    }
    anyhow::bail!(
        "Command `{command}` failed on {} (exit {:?}): {}",
        session.host(),
        result.code,
        result.stderr.trim()
    )
}
