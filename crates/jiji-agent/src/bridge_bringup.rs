//! Native equivalent of restarting `jiji-network-restore-{slug}.service`: renders the same shell
//! script `jiji_network::render_restore_script` used to write to that unit's `restore.sh` and
//! runs it directly from the agent process, with no dependency on that systemd unit. Reusing the
//! exact same script-rendering logic (rather than re-implementing the bridge/iptables sequence
//! natively in Rust) keeps this and `jiji-cli`'s own migration/drift-validation paths
//! (`commands/network/bridge.rs`) from silently diverging over time.

use std::net::Ipv4Addr;
use std::process::Stdio;

use jiji_network::{render_restore_script, BridgeEngineKind, BridgeScriptParams};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::engine::Engine;
use crate::runtime::LocalRuntimeConfig;

fn engine_kind(engine: Engine) -> BridgeEngineKind {
    match engine {
        Engine::Docker => BridgeEngineKind::Docker,
        Engine::Podman => BridgeEngineKind::Podman,
    }
}

pub async fn bring_up_bridge_and_dns(
    engine: Engine,
    wireguard_interface: &str,
    dns_address: Ipv4Addr,
    local: &LocalRuntimeConfig,
) -> Result<(), String> {
    let params = BridgeScriptParams {
        bridge_name: &local.bridge_network,
        bridge_interface: &local.bridge_interface,
        wireguard_interface,
        container_subnet: local.container_subnet,
        bridge_gateway: local.bridge_gateway,
        dns_address,
        container_cidr: local.container_cidr,
        wireguard_port: local.wireguard_port,
        peer_public_ips: &local.peer_public_ips,
        public_host: &local.public_host,
    };
    let script = render_restore_script(engine_kind(engine), &params);
    run_script(&script).await
}

/// Shared with `proxy_bringup.rs`'s own `FORWARD`-chain authorization script -- same "pipe a
/// multi-line `sh` script over stdin" mechanics, no reason to duplicate the spawn/pipe/collect
/// boilerplate for a second unrelated script.
pub(crate) async fn run_script(script: &str) -> Result<(), String> {
    let mut child = Command::new("sh")
        .arg("-s")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not spawn sh to run script: {error}"))?;
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| "script's stdin was not piped".to_string())?;
        stdin
            .write_all(script.as_bytes())
            .await
            .map_err(|error| format!("could not write script: {error}"))?;
    }
    let output = child
        .wait_with_output()
        .await
        .map_err(|error| format!("script failed: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_kind_maps_agent_engine_to_bridge_engine_kind() {
        assert_eq!(engine_kind(Engine::Docker), BridgeEngineKind::Docker);
        assert_eq!(engine_kind(Engine::Podman), BridgeEngineKind::Podman);
    }
}
