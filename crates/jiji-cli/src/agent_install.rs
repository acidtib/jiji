//! Installs and removes the project-scoped `jiji-agent` systemd service on a host. `jiji server
//! setup` treats a missing local binary as a warning, never a hard failure;
//! `jiji server teardown` always removes whatever was actually installed, the same way it treats
//! every other resource.
//!
//! The binary is uploaded the same way every other jiji-managed file is written remotely
//! (`install -m ... /dev/stdin`, piped over the existing exec channel via
//! `execute_with_input` -- see `proxy_ingress.rs::write_remote_file` for the text-only precedent
//! this generalizes to arbitrary bytes), not SFTP: no new remote-file transport is introduced.

use std::path::{Path, PathBuf};

use jiji_agent::{systemd, AgentPaths};
use jiji_config::ContainerEngine;
use jiji_ssh::SshSession;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    Installed,
    Upgraded,
    AlreadyRunning,
}

fn to_agent_engine(engine: ContainerEngine) -> jiji_agent::Engine {
    match engine {
        ContainerEngine::Docker => jiji_agent::Engine::Docker,
        ContainerEngine::Podman => jiji_agent::Engine::Podman,
    }
}

/// Resolves the locally built `jiji-agent` binary to upload. `JIJI_AGENT_BINARY` overrides for
/// tests and custom installs; otherwise looks next to the currently running executable,
/// mirroring how `cargo build` places `jiji`/`jiji_dev`/`jiji-agent` in the same target
/// directory. Returns a plain message (not `anyhow::Error`) because the caller treats "not
/// found" as a warning, not a failure.
pub fn find_local_agent_binary() -> Result<PathBuf, String> {
    if let Ok(path) = std::env::var("JIJI_AGENT_BINARY") {
        let path = PathBuf::from(path);
        return if path.is_file() {
            Ok(path)
        } else {
            Err(format!(
                "JIJI_AGENT_BINARY={} does not exist",
                path.display()
            ))
        };
    }
    let current_exe = std::env::current_exe()
        .map_err(|error| format!("could not resolve the running executable's path: {error}"))?;
    let candidate = current_exe.parent().map(|dir| dir.join("jiji-agent"));
    match candidate {
        Some(candidate) if candidate.is_file() => Ok(candidate),
        _ => Err(
            "no jiji-agent binary found next to the running jiji binary and JIJI_AGENT_BINARY \
             is not set. Build it with `cargo build --release --bin jiji-agent` or set \
             JIJI_AGENT_BINARY to an explicit path."
                .to_string(),
        ),
    }
}

/// Idempotent: skips re-uploading the binary when the remote file's hash already matches (same
/// fingerprint-and-skip shape as `engine::ensure_engine`'s version check), but always
/// re-renders and re-installs the unit file and re-runs `enable --now` so a config-only change
/// (e.g. switching container engines) still takes effect.
pub async fn ensure_agent(
    session: &SshSession,
    engine: ContainerEngine,
    project: &str,
    binary_path: &Path,
    mesh_config: &jiji_agent::runtime::MeshConfig,
    membership: &[jiji_agent::membership::SignedMembership],
) -> anyhow::Result<AgentStatus> {
    let paths = AgentPaths::default_for_project(project);
    let binary = tokio::fs::read(binary_path)
        .await
        .map_err(|error| anyhow::anyhow!("could not read {}: {error}", binary_path.display()))?;
    let local_hash = hex_sha256(&binary);

    let bin_dir = paths
        .binary_path
        .parent()
        .expect("binary path always has a parent directory");
    let setup = format!(
        "install -d -m 0700 {} {} {}",
        paths.project_dir.display(),
        bin_dir.display(),
        paths.state_dir.display(),
    );
    run_required(session, &setup, "create the jiji agent's directories").await?;

    let was_active = is_unit_active(session, &paths.unit_name).await?;
    let remote_hash = remote_sha256(session, &paths.binary_path).await?;
    let uploaded = remote_hash.as_deref() != Some(local_hash.as_str());
    if uploaded {
        write_remote_bytes(session, "0755", &paths.binary_path, &binary).await?;
    }

    let unit = systemd::render_unit(&paths, project, to_agent_engine(engine));
    write_remote_text(
        session,
        "0600",
        &paths.mesh_config_path,
        &serde_json::to_string_pretty(mesh_config)?,
    )
    .await?;
    write_remote_text(
        session,
        "0600",
        &paths.membership_bootstrap_path,
        &serde_json::to_string(membership)?,
    )
    .await?;
    write_remote_text(session, "0644", &paths.unit_path, &unit).await?;

    let command = format!(
        "systemctl stop {unit} >/dev/null 2>&1 || true; \
         {binary} membership-import --project {project} --state-dir {state} \
         --mesh-config {config} --input {bootstrap}; \
         systemctl daemon-reload; systemctl enable --now {unit} >/dev/null; \
         for attempt in 1 2 3 4 5; do {binary} ping --socket {socket} >/dev/null \
         && exit 0; sleep 1; done; exit 1",
        unit = paths.unit_name,
        binary = paths.binary_path.display(),
        state = paths.state_dir.display(),
        config = paths.mesh_config_path.display(),
        bootstrap = paths.membership_bootstrap_path.display(),
        socket = paths.socket_path.display(),
    );
    run_required(session, &command, "start the jiji agent").await?;

    Ok(if !was_active {
        AgentStatus::Installed
    } else if uploaded {
        AgentStatus::Upgraded
    } else {
        AgentStatus::AlreadyRunning
    })
}

/// Stops, disables, and removes the agent's unit, binary, and state -- scoped to this project's
/// own `AgentPaths` only (never a glob over `/etc/jiji/agent`), matching how every other
/// `server teardown` step resolves exact project-derived paths rather than enumerating siblings.
pub async fn remove_agent(session: &SshSession, project: &str) -> anyhow::Result<bool> {
    let paths = AgentPaths::default_for_project(project);
    let was_present = is_unit_active(session, &paths.unit_name).await?
        || path_exists(session, &paths.unit_path).await?;

    let command = format!(
        "systemctl disable --now {unit} >/dev/null 2>&1 || true; \
         rm -f {unit_path}; systemctl daemon-reload; rm -rf {project_dir}",
        unit = paths.unit_name,
        unit_path = paths.unit_path.display(),
        project_dir = paths.project_dir.display(),
    );
    run_required(session, &command, "remove the jiji agent").await?;
    Ok(was_present)
}

async fn is_unit_active(session: &SshSession, unit: &str) -> anyhow::Result<bool> {
    let result = session
        .execute(&format!("systemctl is-active --quiet {unit}"))
        .await?;
    Ok(result.success)
}

async fn path_exists(session: &SshSession, path: &Path) -> anyhow::Result<bool> {
    let result = session
        .execute(&format!("test -e {}", path.display()))
        .await?;
    Ok(result.success)
}

async fn remote_sha256(session: &SshSession, path: &Path) -> anyhow::Result<Option<String>> {
    let command = format!("sha256sum {} 2>/dev/null | cut -d' ' -f1", path.display());
    let result = session.execute(&command).await?;
    let hash = result.stdout.trim();
    Ok((result.success && !hash.is_empty()).then(|| hash.to_string()))
}

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

async fn write_remote_bytes(
    session: &SshSession,
    mode: &str,
    path: &Path,
    content: &[u8],
) -> anyhow::Result<()> {
    let command = format!("install -m {mode} /dev/stdin {}", path.display());
    let result = session.execute_with_input(&command, content).await?;
    if !result.success {
        anyhow::bail!(
            "Could not write {} on {}: {}",
            path.display(),
            session.host(),
            result.stderr.trim()
        );
    }
    Ok(())
}

async fn write_remote_text(
    session: &SshSession,
    mode: &str,
    path: &Path,
    content: &str,
) -> anyhow::Result<()> {
    write_remote_bytes(session, mode, path, content.as_bytes()).await
}

async fn run_required(session: &SshSession, command: &str, action: &str) -> anyhow::Result<()> {
    let result = session.execute(command).await?;
    if !result.success {
        anyhow::bail!(
            "Could not {action} on {}: {}. Fix the host error and retry the command.",
            session.host(),
            result.stderr.trim()
        );
    }
    Ok(())
}
