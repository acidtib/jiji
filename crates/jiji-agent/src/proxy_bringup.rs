//! Native reconciliation of the shared, host-global kamal-proxy container and its Docker-only
//! nftables ingress DNAT rule (Phase 9, replacing `jiji-proxy-ingress-restore.service` and the
//! per-tick dependency on a CLI-driven `ensure_proxy`/`ensure_ingress_rule` SSH call). Creating or
//! recreating the container is the one operation genuinely exclusive across co-resident projects'
//! agents -- concurrent `rm -f` + `run` from two agents could otherwise race into a flapping
//! restart, or a "name already in use" failure -- so it is gated by `host_lease::try_acquire`.
//! Attaching this project's own bridge and applying the ingress DNAT rule are safe unconditionally
//! from any agent, on every tick, without the lease: they only ever add this project's own
//! attachment/target, never touch another project's, and the DNAT rule only ever needs *a*
//! currently-attached address (kamal-proxy listens on every attached interface as one process, so
//! any attached address reaches it) -- never a merged view of every project's routes.

use std::net::Ipv4Addr;
use std::path::Path;
use std::time::Duration;

use tokio::process::Command;

use jiji_network::{BridgeEngineKind, ProxyRunNetwork, CONTAINER_NAME, IMAGE};

use crate::engine::Engine;
use crate::host_lease;
use crate::runtime::MeshConfig;

fn engine_kind(engine: Engine) -> BridgeEngineKind {
    match engine {
        Engine::Docker => BridgeEngineKind::Docker,
        Engine::Podman => BridgeEngineKind::Podman,
    }
}

pub async fn reconcile(engine: Engine, config: &MeshConfig) -> Result<(), String> {
    let fingerprint = jiji_network::config_fingerprint(engine_kind(engine));
    if !is_current_and_running(engine, &fingerprint).await? {
        match host_lease::try_acquire(Path::new(host_lease::DEFAULT_PATH))
            .map_err(|error| format!("could not acquire the host proxy lease: {error}"))?
        {
            Some(_guard) => {
                // Re-check now that the lease is held: another project's agent may have already
                // created/recreated it between the check above and acquiring the lease.
                if !is_current_and_running(engine, &fingerprint).await? {
                    recreate(engine, &fingerprint, config).await?;
                }
            }
            None => return Ok(()), // another project's agent is already handling it this tick
        }
    }
    ensure_attached(engine, config).await?;
    if engine == Engine::Docker {
        let public_host: Ipv4Addr = config
            .local_runtime
            .public_host
            .parse()
            .map_err(|error| format!("public_host is not a valid IPv4 address: {error}"))?;
        apply_ingress_rule(config.local_runtime.proxy_address, public_host).await?;
    }
    Ok(())
}

async fn is_current_and_running(engine: Engine, fingerprint: &str) -> Result<bool, String> {
    let output = Command::new(engine.as_str())
        .args([
            "inspect",
            CONTAINER_NAME,
            "--format",
            "{{.State.Status}} {{index .Config.Labels \"jiji.proxy-config\"}} {{.Config.Image}}",
        ])
        .output()
        .await
        .map_err(|error| error.to_string())?;
    Ok(output.status.success()
        && String::from_utf8_lossy(&output.stdout).trim()
            == format!("running {fingerprint} {IMAGE}"))
}

async fn recreate(engine: Engine, fingerprint: &str, config: &MeshConfig) -> Result<(), String> {
    std::fs::create_dir_all(jiji_network::CERTS_DIR).map_err(|error| error.to_string())?;
    run(engine.as_str(), &["pull", IMAGE]).await?;

    let remove = Command::new(engine.as_str())
        .args(["container", "rm", "-f", CONTAINER_NAME])
        .output()
        .await
        .map_err(|error| error.to_string())?;
    if !remove.status.success()
        && !jiji_network::is_missing_container_error(&String::from_utf8_lossy(&remove.stderr))
    {
        return Err(format!(
            "could not replace kamal-proxy: {}",
            String::from_utf8_lossy(&remove.stderr).trim()
        ));
    }

    let network = ProxyRunNetwork {
        bridge_name: &config.local_runtime.bridge_network,
        proxy_address: config.local_runtime.proxy_address,
    };
    let command =
        jiji_network::render_run_command(engine_kind(engine), Some(&network), fingerprint);
    run_shell(&command).await?;
    wait_until_running(engine).await
}

async fn wait_until_running(engine: Engine) -> Result<(), String> {
    for _ in 0..30 {
        let output = Command::new(engine.as_str())
            .args(["inspect", CONTAINER_NAME, "--format", "{{.State.Status}}"])
            .output()
            .await
            .map_err(|error| error.to_string())?;
        if output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "running" {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err("kamal-proxy did not become ready within 30 seconds".to_string())
}

/// Idempotently attaches kamal-proxy to this project's bridge at its pinned `proxy_address`,
/// additive only -- never touches any other network kamal-proxy might already be attached to for
/// other projects.
async fn ensure_attached(engine: Engine, config: &MeshConfig) -> Result<(), String> {
    let output = Command::new(engine.as_str())
        .args([
            "inspect",
            CONTAINER_NAME,
            "--format",
            "{{json .NetworkSettings.Networks}}",
        ])
        .output()
        .await
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "kamal-proxy is unavailable: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let networks = String::from_utf8_lossy(&output.stdout);
    if let Some(existing) =
        jiji_network::attached_address(&networks, &config.local_runtime.bridge_network)
    {
        if existing == config.local_runtime.proxy_address {
            return Ok(());
        }
        return Err(format!(
            "kamal-proxy is already attached to '{}' with address {existing}, expected {}",
            config.local_runtime.bridge_network, config.local_runtime.proxy_address
        ));
    }
    run(
        engine.as_str(),
        &[
            "network",
            "connect",
            "--ip",
            &config.local_runtime.proxy_address.to_string(),
            &config.local_runtime.bridge_network,
            CONTAINER_NAME,
        ],
    )
    .await
}

async fn apply_ingress_rule(address: Ipv4Addr, public_host: Ipv4Addr) -> Result<(), String> {
    let rules_dir = "/etc/jiji/proxy-ingress";
    std::fs::create_dir_all(rules_dir).map_err(|error| error.to_string())?;
    let rules_path = format!("{rules_dir}/rules.nft");
    std::fs::write(
        &rules_path,
        jiji_network::render_nftables(address, public_host),
    )
    .map_err(|error| error.to_string())?;
    // Pre-creating the table tolerates a cold boot, when it doesn't exist yet and the ruleset's own
    // leading `delete table` line would otherwise fail -- always ignored, matching the CLI's own
    // `2>/dev/null || true` for the identical case.
    let _ = run("nft", &["add", "table", "ip", jiji_network::INGRESS_TABLE]).await;
    run("nft", &["--file", &rules_path]).await
}

async fn run(binary: &str, args: &[&str]) -> Result<(), String> {
    let output = Command::new(binary)
        .args(args)
        .output()
        .await
        .map_err(|error| format!("could not run {binary}: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

async fn run_shell(command: &str) -> Result<(), String> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .output()
        .await
        .map_err(|error| format!("could not run shell command: {error}"))?;
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
