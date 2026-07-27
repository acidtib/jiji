use jiji_config::ContainerEngine;
use jiji_ssh::{CommandResult, SshSession};

use crate::commands::network::setup::{
    network_dir, private_key_path, public_key_path, wireguard_config_path,
};
use crate::container_ops;

fn podman_restart_dropin_path(slug: &str) -> String {
    format!("/etc/systemd/system/podman-restart.service.d/jiji-network-{slug}.conf")
}

pub struct NetworkTeardownStatus {
    pub installed_generation: Option<String>,
    pub other_project_containers: Vec<container_ops::ContainerSummary>,
}

pub async fn discover(
    session: &SshSession,
    engine: ContainerEngine,
    project: &str,
) -> anyhow::Result<NetworkTeardownStatus> {
    let installed_generation = read_installed_generation(session, project).await?;
    let other_project_containers =
        container_ops::list_other_project_containers(session, engine, project).await?;
    Ok(NetworkTeardownStatus {
        installed_generation,
        other_project_containers,
    })
}

async fn read_installed_generation(
    session: &SshSession,
    project: &str,
) -> anyhow::Result<Option<String>> {
    let slug = jiji_network::systemd_unit_slug(project);
    let command = format!("cat {}/generation 2>/dev/null || true", network_dir(&slug));
    let result = session.execute(&command).await?;
    let trimmed = result.stdout.trim();
    Ok(if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    })
}

/// Stops and disables every jiji-authored systemd unit for this project, tolerating units that
/// are already absent or already stopped (mirrors `network/setup.rs`'s own first-install rollback
/// path, which uses the identical `2>/dev/null || true` pattern for the same reason). Only removes
/// this project's own podman-restart drop-in file (one file per project, see `network/setup.rs`)
/// -- other projects' drop-ins, and the shared `podman-restart.service` itself, are untouched.
pub async fn stop_and_disable_units(
    session: &SshSession,
    engine: ContainerEngine,
    project: &str,
) -> anyhow::Result<()> {
    let slug = jiji_network::systemd_unit_slug(project);
    let wireguard_interface = jiji_network::wireguard_interface_name(project);
    let command = format!(
        "systemctl disable --now jiji-dns-{slug}.service jiji-service-nat-{slug}.service \
         jiji-network-restore-{slug}.service wg-quick@{wireguard_interface}.service 2>/dev/null || true; \
         rm -f /etc/systemd/system/jiji-dns-{slug}.service \
         /etc/systemd/system/jiji-service-nat-{slug}.service \
         /etc/systemd/system/jiji-network-restore-{slug}.service"
    );
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)?;

    if engine == ContainerEngine::Podman {
        let command = format!("rm -f {}", podman_restart_dropin_path(&slug));
        let result = session.execute(&command).await?;
        ensure_success(session, &command, &result)?;
    }
    let command = "systemctl daemon-reload";
    let result = session.execute(command).await?;
    ensure_success(session, command, &result)?;
    Ok(())
}

/// `rm -f` is already idempotent on a missing file, so no `|| true` needed here.
pub async fn remove_wireguard(session: &SshSession, project: &str) -> anyhow::Result<()> {
    let slug = jiji_network::systemd_unit_slug(project);
    let wireguard_interface = jiji_network::wireguard_interface_name(project);
    let command = format!(
        "rm -f {} {} {}",
        wireguard_config_path(&wireguard_interface),
        private_key_path(&slug),
        public_key_path(&slug)
    );
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)
}

pub async fn remove_nftables(session: &SshSession, project: &str) -> anyhow::Result<()> {
    let table = jiji_network::service_nat_table_name(project);
    let command = format!("nft delete table ip {table} 2>/dev/null || true");
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)
}

pub enum NetworkRemovalOutcome {
    Removed,
    AlreadyAbsent,
    RetainedAttached(usize),
}

/// Removes this project's bridge/engine network itself, but only when nothing remains attached.
///
/// Deliberately takes no "is kamal-proxy still running" flag: under multi-homing, kamal-proxy
/// being alive for *other* projects is irrelevant to whether *this* bridge can go away, only
/// whether kamal-proxy is still attached to *this specific* bridge is (an earlier version of this
/// function took such a flag, computed from whether other projects still had proxy routes --
/// that was wrong even before multi-homing existed as a concept, since it would retain the bridge
/// forever on any host serving more than one project). Callers must disconnect kamal-proxy from
/// this bridge first (`crate::proxy::disconnect_bridge_if_attached`) if it might be attached;
/// what's left in `attached` here is purely "how many containers are still on this bridge."
pub async fn remove_bridge_and_engine_network(
    session: &SshSession,
    engine: ContainerEngine,
    project: &str,
) -> anyhow::Result<NetworkRemovalOutcome> {
    let bridge_name = jiji_network::bridge_network_name(project);
    let attached =
        container_ops::network_attachment_count(session, engine, &bridge_name, &[]).await?;
    if attached > 0 {
        return Ok(NetworkRemovalOutcome::RetainedAttached(attached));
    }

    let removed = container_ops::remove_network_if_present(session, engine, &bridge_name).await?;
    if engine == ContainerEngine::Podman {
        let bridge_interface = jiji_network::bridge_interface_name(project);
        let command = format!("ip link delete {bridge_interface} type bridge 2>/dev/null || true");
        let result = session.execute(&command).await?;
        ensure_success(session, &command, &result)?;
    }

    if removed {
        Ok(NetworkRemovalOutcome::Removed)
    } else {
        Ok(NetworkRemovalOutcome::AlreadyAbsent)
    }
}

/// Removes this project's own slice of the compiled network state
/// (`/etc/jiji/network/{slug}`) -- never the whole `/etc/jiji/network` tree, which other
/// projects sharing this host may still have their own subtree under. Leaves the jiji sysctl
/// drop-in in place: `net.ipv4.ip_forward=1` is host-global and harmless with zero jiji projects
/// present, so there's no need to track whether another project still needs it.
pub async fn remove_compiled_state(session: &SshSession, project: &str) -> anyhow::Result<()> {
    let command = render_remove_compiled_state_command(project);
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)
}

fn render_remove_compiled_state_command(project: &str) -> String {
    let slug = jiji_network::systemd_unit_slug(project);
    format!("rm -rf {}; systemctl daemon-reload", network_dir(&slug))
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
    fn compiled_state_removal_targets_only_this_projects_network_subdirectory() {
        // Regression guard: this command must always be anchored under /etc/jiji/network/{slug}
        // for *this* project only, never a bare/variable path, and never another project's
        // subtree -- the whole point of scoping this per-project.
        let command = render_remove_compiled_state_command("demo");
        let expected_dir = network_dir(&jiji_network::systemd_unit_slug("demo"));
        assert!(expected_dir.starts_with("/etc/jiji/network/"));
        assert_eq!(
            command,
            format!("rm -rf {expected_dir}; systemctl daemon-reload")
        );

        let other_dir = network_dir(&jiji_network::systemd_unit_slug("other"));
        assert_ne!(expected_dir, other_dir);
        assert!(!command.contains(&other_dir));
    }

    #[test]
    fn unit_disable_command_includes_every_jiji_authored_unit_for_this_project() {
        let slug = jiji_network::systemd_unit_slug("demo");
        let wireguard_interface = jiji_network::wireguard_interface_name("demo");
        let command = format!(
            "systemctl disable --now jiji-dns-{slug}.service jiji-service-nat-{slug}.service \
             jiji-network-restore-{slug}.service wg-quick@{wireguard_interface}.service 2>/dev/null || true; \
             rm -f /etc/systemd/system/jiji-dns-{slug}.service \
             /etc/systemd/system/jiji-service-nat-{slug}.service \
             /etc/systemd/system/jiji-network-restore-{slug}.service"
        );
        for unit in [
            format!("jiji-dns-{slug}.service"),
            format!("jiji-service-nat-{slug}.service"),
            format!("jiji-network-restore-{slug}.service"),
            format!("wg-quick@{wireguard_interface}.service"),
        ] {
            assert!(command.contains(&unit));
        }
        assert!(command.contains("|| true"));
        assert!(command.contains(&format!(
            "rm -f /etc/systemd/system/jiji-dns-{slug}.service"
        )));
    }
}
