use std::net::Ipv4Addr;
use std::time::Duration;

use jiji_config::ContainerEngine;
use jiji_network::CONTAINER_NAME;
use jiji_ssh::SshSession;

fn engine_kind(engine: ContainerEngine) -> jiji_network::BridgeEngineKind {
    match engine {
        ContainerEngine::Docker => jiji_network::BridgeEngineKind::Docker,
        ContainerEngine::Podman => jiji_network::BridgeEngineKind::Podman,
    }
}

/// `ProxyNetwork::public_host` needs a parsed address (used to scope the ingress nftables DNAT to
/// traffic actually addressed to this server), while `ServerPlan::public_host` is a plain `String`
/// (WireGuard enrollment already requires it to be a literal public IPv4 address whenever private
/// networking is enabled, but nothing enforces that when it isn't).
pub(crate) fn parse_public_host(
    server_plan: &jiji_network::ServerPlan,
) -> anyhow::Result<Ipv4Addr> {
    server_plan.public_host.parse().map_err(|_| {
        anyhow::anyhow!(
            "Server '{}' host '{}' must be a public IPv4 address for jiji-proxy's ingress rule",
            server_plan.name,
            server_plan.public_host
        )
    })
}

pub enum ProxyStatus {
    AlreadyRunning,
    Started,
}

/// The private-network address jiji-proxy needs on *this project's* bridge. Absent when
/// `network.enabled` is false, present (and pinned) otherwise. jiji-proxy is a single, shared,
/// per-host container that becomes multi-homed across every project's bridge that has active
/// routes on that host -- see `ensure_proxy`'s doc comment.
#[derive(Debug, Clone)]
pub struct ProxyNetwork {
    pub bridge_name: String,
    pub bridge_interface: String,
    pub proxy_address: Ipv4Addr,
    pub dns_address: Ipv4Addr,
    /// This server's own public IP -- the ingress nftables rule restricts its DNAT to traffic
    /// actually addressed here, so it never hijacks cross-host mesh traffic merely passing through
    /// this host's `prerouting` hook on its way to a different peer (see `proxy_ingress.rs`).
    pub public_host: Ipv4Addr,
}

/// Ensures jiji-proxy is running with the current image/engine, and (if `network` is given)
/// attached to this project's bridge at its pinned `proxy_address`.
///
/// jiji-proxy is the one deliberately shared, multi-tenant, per-host component in jiji's
/// otherwise per-project-isolated network design (see the project's network-isolation notes): one
/// container, routes namespaced per project, serving every project on a host at once. Under that
/// design it must become **multi-homed** -- attached to every project's bridge that has routes on
/// this host, not just the most recent one. That means "does the container need replacing"
/// (image/engine drift, decided by `config_fingerprint`/`is_current_and_running`) and "is this
/// project's network attached" (`ensure_attached`, additive, idempotent) have to be two
/// independent steps: recreating the container just because a *different* project's `ensure_proxy`
/// call ran would tear down every other project's attachment too.
///
/// jiji-proxy is deliberately given no `--dns`/`--dns-search` here (unlike a project's own service
/// containers): it does its own DNS resolution against whichever `dns_server` a pushed route names
/// (see `proxy_routes.rs`), which is only ever a specific `.jiji`-serving jiji-agent address, never
/// the container's own default resolver. This also sidesteps a real multi-homing problem: a single
/// resolv.conf pointed at multiple projects' jiji-agent DNS resolvers wouldn't reliably resolve
/// names across all of them (a resolver conventionally stops at the first definitive NXDOMAIN
/// rather than falling through to the next nameserver), and per-project DNS can't be changed after
/// container creation via `network connect` anyway.
pub async fn ensure_proxy(
    session: &SshSession,
    engine: ContainerEngine,
    network: Option<ProxyNetwork>,
    force: bool,
) -> anyhow::Result<ProxyStatus> {
    let fingerprint = jiji_network::config_fingerprint(engine_kind(engine));
    upload_daemon_config(session).await?;
    let status = if !force && is_current_and_running(session, engine, &fingerprint).await? {
        ProxyStatus::AlreadyRunning
    } else {
        recreate(session, engine, &fingerprint, network.as_ref()).await?;
        ProxyStatus::Started
    };

    // Always idempotent, even right after `recreate` attached this exact network as primary --
    // `ensure_attached` inspects first and no-ops when it's already correctly attached, so there
    // is no need to special-case "did we just create it with this network already."
    if let Some(network) = &network {
        ensure_attached(session, engine, network).await?;
        crate::commands::network::bridge::reconcile_podman_dns_address(
            session,
            engine,
            &network.bridge_interface,
            network.dns_address,
        )
        .await?;
        // Docker only: see `proxy_ingress` for why `--publish` alone isn't enough on jiji's
        // bridge networks. Re-applied on every call, from any project sharing this host, so it
        // self-heals and always targets a currently-attached address. TCP-route ports are always
        // `&[]` here (this is only the one-time synchronous priming step) -- see
        // `ensure_ingress_rule`'s own doc comment for why the ongoing source of truth is
        // jiji-agent's own reconcile tick instead.
        if engine == ContainerEngine::Docker {
            crate::proxy_ingress::ensure_ingress_rule(
                session,
                network.proxy_address,
                network.public_host,
                true,
                &[],
            )
            .await?;
        }
    }

    crate::proxy_routes::check_version(session, engine, session.host()).await?;

    Ok(status)
}

/// Renders and uploads jiji-proxy's fixed, convention-based daemon config (see
/// `jiji_network::render_daemon_config`) to `{CONFIG_DIR}/config.yml`, idempotently, on every
/// `ensure_proxy` call regardless of whether the container itself needs replacing -- the config
/// has no per-host state, so re-rendering it is always safe, and it must be in place *before*
/// `recreate` ever starts a fresh container (which mounts `CONFIG_DIR` read-only).
async fn upload_daemon_config(session: &SshSession) -> anyhow::Result<()> {
    let content = jiji_network::render_daemon_config();
    let remote_path = format!("{}/config.yml", jiji_network::CONFIG_DIR);
    let temp = format!("{remote_path}.jiji-new");
    let command = format!("set -eu; install -D -m 0644 /dev/stdin {temp}; mv {temp} {remote_path}");
    let result = session
        .execute_with_input(&command, content.as_bytes())
        .await?;
    if !result.success {
        anyhow::bail!(
            "Could not write jiji-proxy config on {}: {}",
            session.host(),
            result.stderr.trim()
        );
    }
    Ok(())
}

async fn recreate(
    session: &SshSession,
    engine: ContainerEngine,
    fingerprint: &str,
    network: Option<&ProxyNetwork>,
) -> anyhow::Result<()> {
    run_required(
        session,
        &format!("mkdir -p {}", jiji_network::CERTS_DIR),
        "create certificate directory",
    )
    .await?;
    run_required(
        session,
        &format!("{engine} pull {}", jiji_network::IMAGE),
        "pull jiji-proxy image",
    )
    .await?;

    let run_network = network.map(|network| jiji_network::ProxyRunNetwork {
        bridge_name: &network.bridge_name,
        proxy_address: network.proxy_address,
    });
    let run_command =
        jiji_network::render_run_command(engine_kind(engine), run_network.as_ref(), fingerprint);
    // Removing and recreating the container is wrapped in the exact same host-local flock
    // jiji-agent's own `host_lease` module uses (`crates/jiji-agent/src/host_lease.rs`), as one
    // combined remote shell invocation rather than two separate SSH round-trips -- confirmed live:
    // without this, jiji-agent's own continuous reconcile loop (`proxy_bringup.rs`, which starts
    // ticking the moment the agent's systemd unit comes up, before this CLI-driven step even runs)
    // can race this exact rm-then-run sequence, since the agent only coordinates against *other
    // agents* sharing the lease, never against a concurrent CLI-driven SSH command -- the two
    // `podman run --name jiji-proxy` invocations then corrupt the container's overlay storage
    // (`directory not empty` on every subsequent removal attempt, not just a transient "name
    // already in use"). A blocking flock across the whole rm+run sequence, not just each command
    // individually, is required: releasing and reacquiring the lock between two separate SSH calls
    // would still leave a window for the agent's own recreate (guarded by the same lock, but only
    // for its own duration) to run in between. `rm -f`'s own failure is tolerated unconditionally
    // here (`|| true`) rather than checked against `is_missing_container_error`, since a real
    // removal problem still surfaces as a `run` failure right after.
    let combined = format!(
        "mkdir -p $(dirname {lock_path}); flock --timeout 60 {lock_path} -c 'set -eu; {engine} container rm -f {CONTAINER_NAME} >/dev/null 2>&1 || true; {run_command}'",
        lock_path = jiji_agent::host_lease::DEFAULT_PATH,
    );
    let result = session.execute(&combined).await?;
    if !result.success {
        anyhow::bail!(
            "Could not replace jiji-proxy on {}: {}. Remove the existing '{}' container and retry the command.",
            session.host(),
            result.stderr.trim(),
            CONTAINER_NAME
        );
    }
    wait_until_running(session, engine).await
}

/// Idempotently attaches jiji-proxy to `network.bridge_name` at `network.proxy_address`, additive
/// only -- never touches any other network jiji-proxy might already be attached to for other
/// projects. Already-attached-with-the-expected-address is a silent no-op; already-attached-with-
/// a-different-address is a hard error (the same class of failure as an image/engine fingerprint
/// mismatch, since it means something changed out from under jiji).
async fn ensure_attached(
    session: &SshSession,
    engine: ContainerEngine,
    network: &ProxyNetwork,
) -> anyhow::Result<()> {
    let command = format!(
        "{engine} inspect {CONTAINER_NAME} --format '{{{{json .NetworkSettings.Networks}}}}'"
    );
    let result = session.execute(&command).await?;
    if !result.success {
        anyhow::bail!(
            "Could not inspect jiji-proxy's attached networks on {}: {}",
            session.host(),
            result.stderr.trim()
        );
    }

    if let Some(existing) = jiji_network::attached_address(&result.stdout, &network.bridge_name) {
        if existing == network.proxy_address {
            return Ok(());
        }
        anyhow::bail!(
            "jiji-proxy on {} is already attached to network '{}' with address {existing}, expected {}. Remove the container with `{engine} rm -f {CONTAINER_NAME}` and retry, or investigate the address drift.",
            session.host(),
            network.bridge_name,
            network.proxy_address
        );
    }

    let command = format!(
        "{engine} network connect --ip {} {} {CONTAINER_NAME}",
        network.proxy_address, network.bridge_name
    );
    let result = session.execute(&command).await?;
    if !result.success {
        anyhow::bail!(
            "Could not attach jiji-proxy to network '{}' on {}: {}. Run `jiji network setup` for this project and retry.",
            network.bridge_name,
            session.host(),
            result.stderr.trim()
        );
    }
    Ok(())
}

async fn is_current_and_running(
    session: &SshSession,
    engine: ContainerEngine,
    fingerprint: &str,
) -> anyhow::Result<bool> {
    let result = session
        .execute(&format!(
            "{engine} inspect {CONTAINER_NAME} --format '{{{{.State.Status}}}} {{{{index .Config.Labels \"jiji.proxy-config\"}}}} {{{{.Config.Image}}}}'"
        ))
        .await?;
    Ok(result.success
        && result.stdout.trim() == format!("running {fingerprint} {}", jiji_network::IMAGE))
}

async fn wait_until_running(session: &SshSession, engine: ContainerEngine) -> anyhow::Result<()> {
    for _ in 0..30 {
        let result = session
            .execute(&format!(
                "{engine} inspect {CONTAINER_NAME} --format '{{{{.State.Status}}}}'"
            ))
            .await?;
        if result.success && result.stdout.trim() == "running" {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    let logs = session
        .execute(&format!("{engine} logs --tail 20 {CONTAINER_NAME}"))
        .await?;
    anyhow::bail!(
        "jiji-proxy did not become ready on {} within 30 seconds. Inspect it with `{engine} logs {CONTAINER_NAME}` and retry the command. Recent logs: {}",
        session.host(),
        logs.stdout.trim()
    )
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

/// Disconnects jiji-proxy from `bridge_name` if it's attached, tolerating "jiji-proxy doesn't
/// exist" and "not attached to this network" as success (returning `false`, not an error). Used
/// by `commands/server/teardown.rs` before removing a project's bridge network -- jiji-proxy may
/// still be attached to *other* projects' bridges and must keep running for them. Returns whether
/// a disconnect actually happened, matching this crate's `present_or_absent` step-reporting idiom.
pub async fn disconnect_bridge_if_attached(
    session: &SshSession,
    engine: ContainerEngine,
    bridge_name: &str,
) -> anyhow::Result<bool> {
    let command = format!("{engine} network disconnect {bridge_name} {CONTAINER_NAME}");
    let result = session.execute(&command).await?;
    if result.success {
        return Ok(true);
    }
    if jiji_network::is_missing_container_error(&result.stderr) {
        return Ok(false);
    }
    let stderr = result.stderr.to_ascii_lowercase();
    if stderr.contains("is not connected") || stderr.contains("not found") {
        return Ok(false);
    }
    anyhow::bail!(
        "Could not disconnect jiji-proxy from network '{bridge_name}' on {}: {}",
        session.host(),
        result.stderr.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn network(bridge_name: &str, proxy_address: &str) -> ProxyNetwork {
        ProxyNetwork {
            bridge_name: bridge_name.to_string(),
            bridge_interface: "jijib1234567".to_string(),
            proxy_address: proxy_address.parse().unwrap(),
            dns_address: "10.0.2.2".parse().unwrap(),
            public_host: "203.0.113.10".parse().unwrap(),
        }
    }

    // `render_run_command`/`attached_address` moved to `jiji_network::proxy_script` (Phase 9,
    // shared with the agent's native reconciliation) and are tested there.

    #[test]
    fn network_test_helper_still_builds_the_expected_struct() {
        let net = network("jiji-demo-9f8e7d6c", "10.0.2.9");
        assert_eq!(net.bridge_name, "jiji-demo-9f8e7d6c");
        assert_eq!(net.bridge_interface, "jijib1234567");
        assert_eq!(net.proxy_address, "10.0.2.9".parse::<Ipv4Addr>().unwrap());
        assert_eq!(net.dns_address, "10.0.2.2".parse::<Ipv4Addr>().unwrap());
    }
}
