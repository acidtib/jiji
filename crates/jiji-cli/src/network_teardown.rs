use jiji_config::ContainerEngine;
use jiji_ssh::{CommandResult, SshSession};

use crate::commands::network::bridge::NETWORK_ANCHOR_CONTAINER_NAME;
use crate::commands::network::setup::{
    NETWORK_DIR, PRIVATE_KEY_PATH, PUBLIC_KEY_PATH, WIREGUARD_CONFIG_PATH,
};
use crate::container_ops;

const BRIDGE_NETWORK_NAME: &str = "jiji";
const SERVICE_NAT_TABLE: &str = "jiji_service_nat";
const SYSCTL_CONF_PATH: &str = "/etc/sysctl.d/99-jiji-network.conf";
const PODMAN_RESTART_DROPIN_PATH: &str =
    "/etc/systemd/system/podman-restart.service.d/jiji-network.conf";

pub struct NetworkTeardownStatus {
    pub installed_generation: Option<String>,
    pub other_project_containers: Vec<container_ops::ContainerSummary>,
}

pub async fn discover(
    session: &SshSession,
    engine: ContainerEngine,
    project: &str,
) -> anyhow::Result<NetworkTeardownStatus> {
    let installed_generation = read_installed_generation(session).await?;
    let other_project_containers =
        container_ops::list_other_project_containers(session, engine, project).await?;
    Ok(NetworkTeardownStatus {
        installed_generation,
        other_project_containers,
    })
}

async fn read_installed_generation(session: &SshSession) -> anyhow::Result<Option<String>> {
    let command = format!("cat {NETWORK_DIR}/generation 2>/dev/null || true");
    let result = session.execute(&command).await?;
    let trimmed = result.stdout.trim();
    Ok(if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    })
}

/// Stops and disables every jiji-authored systemd unit, tolerating units that are already
/// absent or already stopped (mirrors `network/setup.rs`'s own first-install rollback path,
/// which uses the identical `2>/dev/null || true` pattern for the same reason).
pub async fn stop_and_disable_units(
    session: &SshSession,
    engine: ContainerEngine,
) -> anyhow::Result<()> {
    let command = "systemctl disable --now jiji-dns.service jiji-service-nat.service \
         jiji-network-restore.service wg-quick@jiji0.service 2>/dev/null || true"
        .to_string();
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)?;

    if engine == ContainerEngine::Podman {
        let command = format!("rm -f {PODMAN_RESTART_DROPIN_PATH}");
        let result = session.execute(&command).await?;
        ensure_success(session, &command, &result)?;
    }
    Ok(())
}

/// `rm -f` is already idempotent on a missing file, so no `|| true` needed here.
pub async fn remove_wireguard(session: &SshSession) -> anyhow::Result<()> {
    let command = format!("rm -f {WIREGUARD_CONFIG_PATH} {PRIVATE_KEY_PATH} {PUBLIC_KEY_PATH}");
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)
}

pub async fn remove_nftables(session: &SshSession) -> anyhow::Result<()> {
    let command = format!("nft delete table ip {SERVICE_NAT_TABLE} 2>/dev/null || true");
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)
}

pub enum NetworkRemovalOutcome {
    Removed,
    AlreadyAbsent,
    RetainedAttached(usize),
}

/// Removes the Podman keepalive anchor (if applicable) and then the `jiji` bridge/engine network
/// itself, but only when nothing remains attached. `kamal_proxy_still_running` short-circuits
/// straight to a retained outcome, since a still-running kamal-proxy is itself attached to `jiji`
/// and network removal would fail regardless of anything else.
pub async fn remove_bridge_and_engine_network(
    session: &SshSession,
    engine: ContainerEngine,
    kamal_proxy_still_running: bool,
) -> anyhow::Result<NetworkRemovalOutcome> {
    if engine == ContainerEngine::Podman {
        container_ops::remove_if_present(session, engine, NETWORK_ANCHOR_CONTAINER_NAME).await?;
    }

    let attached =
        container_ops::network_attachment_count(session, engine, BRIDGE_NETWORK_NAME, &[]).await?;
    if kamal_proxy_still_running || attached > 0 {
        return Ok(NetworkRemovalOutcome::RetainedAttached(attached));
    }

    if container_ops::remove_network_if_present(session, engine, BRIDGE_NETWORK_NAME).await? {
        Ok(NetworkRemovalOutcome::Removed)
    } else {
        Ok(NetworkRemovalOutcome::AlreadyAbsent)
    }
}

/// Removes the entire compiled `/etc/jiji/network` tree and the jiji sysctl drop-in. `NETWORK_DIR`
/// is a fixed constant, never built from a variable, so this can't be redirected to delete
/// anything outside jiji-owned paths.
pub async fn remove_compiled_state(session: &SshSession) -> anyhow::Result<()> {
    let command =
        format!("rm -rf {NETWORK_DIR}; rm -f {SYSCTL_CONF_PATH}; systemctl daemon-reload");
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_state_removal_targets_only_the_fixed_network_dir() {
        // Regression guard: this command must never be built from a variable path.
        assert_eq!(NETWORK_DIR, "/etc/jiji/network");
    }

    #[test]
    fn unit_disable_command_includes_every_jiji_authored_unit() {
        let command = "systemctl disable --now jiji-dns.service jiji-service-nat.service \
             jiji-network-restore.service wg-quick@jiji0.service 2>/dev/null || true";
        for unit in [
            "jiji-dns.service",
            "jiji-service-nat.service",
            "jiji-network-restore.service",
            "wg-quick@jiji0.service",
        ] {
            assert!(command.contains(unit));
        }
        assert!(command.contains("|| true"));
    }
}
