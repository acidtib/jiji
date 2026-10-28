//! Docker-only workaround for a confirmed live-host bug: jiji-proxy's `--publish 80:8080
//! --publish 443:8443` (`proxy.rs::run_command`) is silently skipped for IPv4 when the
//! container's primary network is one of jiji's own bridges, because those bridges are created
//! with `--opt com.docker.network.bridge.enable_ip_masquerade=false --opt
//! com.docker.network.bridge.gateway_mode_ipv4=routed` (`commands/network/bridge.rs`, needed so
//! backend containers get real routable addresses across the WireGuard mesh instead of NAT'd
//! ones). Reproduced with a minimal `docker network create` + `--publish` container outside any
//! jiji code: dockerd logs "Host port ignored, because NAT is disabled" and the IPv4 host port
//! never binds, while the IPv6 publish still works. Podman's bridge creation
//! (`commands/network/bridge.rs`) doesn't set either option, so Podman's `--publish` is
//! unaffected and never calls into this module.
//!
//! The fix bypasses Docker's own port-publish machinery entirely: a host-level nftables DNAT
//! rule forwards the public ports straight to jiji-proxy's bridge address, which is directly
//! reachable from the host regardless of the bridge's masquerade/gateway-mode settings. This is
//! host-global, not per-project (unlike `service_network.rs`'s VIP mappings), because jiji-proxy
//! itself is the one shared, multi-tenant component on a host -- any project's `ensure_proxy` call
//! re-applies the same rule, idempotently, targeting whichever project's address it was just
//! given (jiji-proxy listens on every attached interface, so any currently-attached address
//! reaches the same process).
//!
//! Before Phase 9, boot persistence for this rule (nftables state doesn't survive a reboot on its
//! own) was a dedicated `jiji-proxy-ingress-restore.service` unit this module installed and
//! enabled. Since Phase 9, every project's `jiji-agent` reconciles this same rule continuously
//! (`jiji-agent/src/proxy_bringup.rs`, using the identical `jiji_network::render_nftables`), so as
//! long as at least one project's agent is running on a host, the rule converges on its own within
//! one reconcile tick -- no separate persistence unit is installed for new hosts. `ensure_ingress_rule`
//! here remains for `jiji server setup`/`jiji proxy restart`'s own synchronous, on-demand apply
//! (the same "one-time synchronous priming plus continuous background reconciliation" pattern used
//! for the bridge/WireGuard bring-up, see `commands/network/setup.rs`); `remove_ingress_rule`'s
//! unit cleanup is migration-only, for hosts provisioned before Phase 9.

use std::net::Ipv4Addr;

use jiji_config::ContainerEngine;
use jiji_network::{CONTAINER_NAME, INGRESS_TABLE};
use jiji_ssh::SshSession;

const RULES_DIR: &str = jiji_network::INGRESS_RULES_DIR;
const RULES_PATH: &str = "/etc/jiji/proxy-ingress/rules.nft";
const RESTORE_SCRIPT_PATH: &str = "/etc/jiji/proxy-ingress/restore.sh";
const UNIT_PATH: &str = "/etc/systemd/system/jiji-proxy-ingress-restore.service";
const UNIT_NAME: &str = "jiji-proxy-ingress-restore.service";

/// Idempotent: safe to call on every `ensure_proxy`, from any project sharing this host. Only the
/// host-global HTTP table; TCP-route ports live in each project's own table, which only that
/// project's agent writes.
///
/// `include_http`: whether to DNAT 80/443 at all -- `false` on Podman, whose own bridges already
/// publish those ports natively. The `FORWARD`-chain authorization is unconditional on every
/// engine, since jiji-proxy always listens on both internal HTTP ports.
pub async fn ensure_ingress_rule(
    session: &SshSession,
    address: Ipv4Addr,
    public_host: Ipv4Addr,
    include_http: bool,
) -> anyhow::Result<()> {
    write_remote_file(
        session,
        &format!("mkdir -p {RULES_DIR}"),
        "0644",
        RULES_PATH,
        &jiji_network::render_nftables(INGRESS_TABLE, address, public_host, include_http, &[]),
    )
    .await?;

    // Pre-creating the table tolerates a cold boot, when it doesn't exist yet and the ruleset's
    // own leading `delete table` line would otherwise fail.
    let command = format!(
        "set -eu; nft add table ip {INGRESS_TABLE} 2>/dev/null || true; nft --file {RULES_PATH}"
    );
    run_required(
        session,
        &command,
        "apply the jiji-proxy public ingress rule",
    )
    .await?;

    let forward_script = jiji_network::render_forward_accept_script(address, &[]);
    run_required(
        session,
        &forward_script,
        "authorize the jiji-proxy ingress rule in the engine's own FORWARD chain",
    )
    .await?;
    Ok(())
}

/// Used only when jiji-proxy's own container is removed (no project has routes left) --
/// tolerates the rule already being absent. The unit/script cleanup is migration-only: new hosts
/// never install them (Phase 9), but a host provisioned before Phase 9 may still have them.
pub async fn remove_ingress_rule(session: &SshSession) -> anyhow::Result<()> {
    let command = format!(
        "systemctl disable --now {UNIT_NAME} >/dev/null 2>&1 || true; \
         rm -f {UNIT_PATH} {RESTORE_SCRIPT_PATH}; systemctl daemon-reload; \
         nft delete table ip {INGRESS_TABLE} 2>/dev/null || true; rm -rf {RULES_DIR}"
    );
    run_required(
        session,
        &command,
        "remove the jiji-proxy public ingress rule",
    )
    .await
}

/// Removes this project's TCP-route tables from the host and, when jiji-proxy is still running
/// for other projects, from its network namespace. Tolerates either being absent.
pub async fn remove_project_tcp_tables(
    session: &SshSession,
    engine: ContainerEngine,
    project: &str,
) -> anyhow::Result<()> {
    let ingress_table = jiji_network::tcp_ingress_table_name(project);
    let relay_table = jiji_network::tcp_relay_table_name(project);
    let command = format!(
        "nft delete table ip {ingress_table} 2>/dev/null || true; \
         rm -f {RULES_DIR}/{ingress_table}.nft; \
         pid=$({engine} inspect {CONTAINER_NAME} --format '{{{{.State.Pid}}}}' 2>/dev/null || true); \
         if [ -n \"$pid\" ] && [ \"$pid\" != 0 ]; then \
         nsenter --net=/proc/$pid/ns/net -- nft delete table ip {relay_table} 2>/dev/null || true; \
         fi"
    );
    run_required(
        session,
        &command,
        "remove this project's jiji-proxy TCP route tables",
    )
    .await
}

pub async fn refresh_from_surviving_attachment(
    session: &SshSession,
    engine: ContainerEngine,
    public_host: Ipv4Addr,
) -> anyhow::Result<bool> {
    if engine != ContainerEngine::Docker {
        return Ok(false);
    }
    let command = format!(
        "docker inspect --format '{{{{range $name, $network := .NetworkSettings.Networks}}}}{{{{printf \"%s %s\\n\" $name $network.IPAddress}}}}{{{{end}}}}' {} 2>/dev/null || true",
        jiji_network::CONTAINER_NAME
    );
    let result = session.execute(&command).await?;
    let address = jiji_network::surviving_proxy_address(&result.stdout);
    if let Some(address) = address {
        // Only ever reached when `engine == Docker` (checked above), so the HTTP lines belong here.
        ensure_ingress_rule(session, address, public_host, true).await?;
        return Ok(true);
    }
    Ok(false)
}

async fn write_remote_file(
    session: &SshSession,
    setup: &str,
    mode: &str,
    path: &str,
    content: &str,
) -> anyhow::Result<()> {
    let result = session.execute(setup).await?;
    if !result.success {
        anyhow::bail!(
            "Could not prepare {path} on {}: {}",
            session.host(),
            result.stderr.trim()
        );
    }
    let command = format!("install -m {mode} /dev/stdin {path}");
    let result = session
        .execute_with_input(&command, content.as_bytes())
        .await?;
    if !result.success {
        anyhow::bail!(
            "Could not write {path} on {}: {}",
            session.host(),
            result.stderr.trim()
        );
    }
    Ok(())
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

// `render_nftables`/`surviving_proxy_address` moved to `jiji_network::proxy_script` (Phase 9,
// shared with the agent's native reconciliation) and are tested there.
