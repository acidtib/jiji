//! Native reconciliation of the shared, host-global jiji-proxy container and its Docker-only
//! nftables ingress DNAT rule (Phase 9, replacing `jiji-proxy-ingress-restore.service` and the
//! per-tick dependency on a CLI-driven `ensure_proxy`/`ensure_ingress_rule` SSH call). Creating or
//! recreating the container is the one operation genuinely exclusive across co-resident projects'
//! agents -- concurrent `rm -f` + `run` from two agents could otherwise race into a flapping
//! restart, or a "name already in use" failure -- so it is gated by `host_lease::try_acquire`.
//! Attaching this project's own bridge and applying the ingress DNAT rule are safe unconditionally
//! from any agent, on every tick, without the lease: they only ever add this project's own
//! attachment/target, never touch another project's, and the DNAT rule only ever needs *a*
//! currently-attached address (jiji-proxy listens on every attached interface as one process, so
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
    // No per-host state, so it's always safe to re-render and overwrite -- and it must be in
    // place before `recreate` ever starts a fresh container, which mounts CONFIG_DIR read-only.
    upload_daemon_config()?;

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
    // The desired listen_port set, not a live query against jiji-proxy's own route table:
    // `local_reconcile::reconcile_tcp_routes` (run every tick, same as this function) keeps
    // jiji-proxy's actual TCP routes converged to this exact same config-derived list, so it's
    // already the correct source of truth for which public ports need a DNAT rule.
    let tcp_ports: Vec<u16> = config
        .local_runtime
        .tcp_routes
        .iter()
        .map(|route| route.listen_port)
        .collect();
    // The HTTP 80/443 DNAT lines are Docker-only (Podman's own bridges already publish those
    // natively -- see `render_nftables`'s doc comment), but a TCP route's public port has no such
    // native alternative on either engine: `--publish` can only ever publish a fixed port set
    // declared at container-creation time, incompatible with adding a route without ever
    // restarting jiji-proxy. So this whole step now runs on Podman too whenever there's at least
    // one TCP route to expose, not only on Docker -- confirmed live: without this, a Podman host's
    // TCP routes were pushed into jiji-proxy's own route table successfully but were never actually
    // reachable from outside the host at all, since nothing ever published or forwarded the public
    // port to jiji-proxy's internal TCP relay listener.
    if engine == Engine::Docker || !tcp_ports.is_empty() {
        let public_host: Ipv4Addr = config
            .local_runtime
            .public_host
            .parse()
            .map_err(|error| format!("public_host is not a valid IPv4 address: {error}"))?;
        apply_ingress_rule(
            config.local_runtime.proxy_address,
            public_host,
            engine == Engine::Docker,
            &tcp_ports,
        )
        .await?;
    }

    // Unconditional on every engine, unlike the DNAT table above: jiji-proxy always listens on its
    // two internal HTTP ports regardless of route configuration, and this hook drops genuinely
    // external traffic to them regardless of which mechanism -- our own DNAT workaround, or an
    // engine's native `--publish` -- delivered the packet here (confirmed live: Podman's own
    // *native* HTTP publish path was silently dropped the exact same way a TCP route's DNAT was,
    // not just traffic through our own added ingress table; see `render_forward_accept_script`'s
    // own doc comment).
    let forward_script =
        jiji_network::render_forward_accept_script(config.local_runtime.proxy_address, &tcp_ports);
    crate::bridge_bringup::run_script(&forward_script).await?;

    // The host-side ingress rule above only rewrites the destination *address*, preserving a TCP
    // route's own port (see `jiji_network::render_nftables`'s doc comment): the remap to
    // jiji-proxy's fixed internal relay port must happen a second time, inside jiji-proxy's own
    // container network namespace, or `SO_ORIGINAL_DST` inside jiji-proxy has nothing to recover
    // (confirmed live -- a rewrite applied in the host's namespace comes back `ENOENT` when queried
    // from the container's separate one). Always re-applied here (even with an empty `tcp_ports`,
    // which just clears the table) so a route removed since the last tick doesn't leave a stale
    // in-netns rewrite behind.
    let pid = container_pid(engine).await?;
    apply_relay_netns_nat(pid, &tcp_ports).await?;
    Ok(())
}

/// jiji-proxy's own container PID as seen from the host -- the PID `nsenter --net=/proc/{pid}/ns/net`
/// needs to join its network namespace. Only ever called once `reconcile` has already confirmed the
/// container is running (via `is_current_and_running`/`wait_until_running` above).
async fn container_pid(engine: Engine) -> Result<u32, String> {
    let output = Command::new(engine.as_str())
        .args(["inspect", CONTAINER_NAME, "--format", "{{.State.Pid}}"])
        .output()
        .await
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "could not read jiji-proxy's PID: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u32>()
        .map_err(|error| format!("jiji-proxy reported a non-numeric PID: {error}"))
}

async fn apply_relay_netns_nat(pid: u32, tcp_ports: &[u16]) -> Result<(), String> {
    let script = jiji_network::render_relay_netns_apply_script(pid, tcp_ports);
    crate::bridge_bringup::run_script(&script).await
}

fn upload_daemon_config() -> Result<(), String> {
    std::fs::create_dir_all(jiji_network::CONFIG_DIR).map_err(|error| error.to_string())?;
    let path = format!("{}/config.yml", jiji_network::CONFIG_DIR);
    std::fs::write(&path, jiji_network::render_daemon_config()).map_err(|error| error.to_string())
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
            "could not replace jiji-proxy: {}",
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
    Err("jiji-proxy did not become ready within 30 seconds".to_string())
}

/// Idempotently attaches jiji-proxy to this project's bridge at its pinned `proxy_address`,
/// additive only -- never touches any other network jiji-proxy might already be attached to for
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
            "jiji-proxy is unavailable: {}",
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
            "jiji-proxy is already attached to '{}' with address {existing}, expected {}",
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

async fn apply_ingress_rule(
    address: Ipv4Addr,
    public_host: Ipv4Addr,
    include_http: bool,
    tcp_ports: &[u16],
) -> Result<(), String> {
    let rules_dir = "/etc/jiji/proxy-ingress";
    std::fs::create_dir_all(rules_dir).map_err(|error| error.to_string())?;
    let rules_path = format!("{rules_dir}/rules.nft");
    std::fs::write(
        &rules_path,
        jiji_network::render_nftables(address, public_host, include_http, tcp_ports),
    )
    .map_err(|error| error.to_string())?;
    // Pre-creating the table tolerates a cold boot, when it doesn't exist yet and the ruleset's own
    // leading `delete table` line would otherwise fail -- always ignored, matching the CLI's own
    // `2>/dev/null || true` for the identical case.
    let _ = run("nft", &["add", "table", "ip", jiji_network::INGRESS_TABLE]).await;
    run("nft", &["--file", &rules_path]).await?;
    Ok(())
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
