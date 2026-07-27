//! Docker-only workaround for a confirmed live-host bug: kamal-proxy's `--publish 80:8080
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
//! rule forwards the public ports straight to kamal-proxy's bridge address, which is directly
//! reachable from the host regardless of the bridge's masquerade/gateway-mode settings. This is
//! host-global, not per-project (unlike `service_network.rs`'s VIP mappings), because kamal-proxy
//! itself is the one shared, multi-tenant component on a host -- any project's `ensure_proxy` call
//! re-applies the same rule, idempotently, targeting whichever project's address it was just
//! given (kamal-proxy listens on every attached interface, so any currently-attached address
//! reaches the same process).

use std::net::Ipv4Addr;

use jiji_ssh::SshSession;

use crate::proxy::{INTERNAL_HTTPS_PORT, INTERNAL_HTTP_PORT};

const TABLE: &str = "jiji_proxy_ingress";
const RULES_DIR: &str = "/etc/jiji/proxy-ingress";
const RULES_PATH: &str = "/etc/jiji/proxy-ingress/rules.nft";
const RESTORE_SCRIPT_PATH: &str = "/etc/jiji/proxy-ingress/restore.sh";
const UNIT_PATH: &str = "/etc/systemd/system/jiji-proxy-ingress-restore.service";
const UNIT_NAME: &str = "jiji-proxy-ingress-restore.service";

pub fn render_nftables(address: Ipv4Addr) -> String {
    format!(
        "delete table ip {TABLE}\n\
         table ip {TABLE} {{\n\
         \tchain prerouting {{\n\
         \t\ttype nat hook prerouting priority dstnat - 5; policy accept;\n\
         \t\ttcp dport 80 dnat to {address}:{INTERNAL_HTTP_PORT}\n\
         \t\ttcp dport 443 dnat to {address}:{INTERNAL_HTTPS_PORT}\n\
         \t}}\n\
         \tchain output {{\n\
         \t\ttype nat hook output priority dstnat - 5; policy accept;\n\
         \t\ttcp dport 80 dnat to {address}:{INTERNAL_HTTP_PORT}\n\
         \t\ttcp dport 443 dnat to {address}:{INTERNAL_HTTPS_PORT}\n\
         \t}}\n\
         }}\n"
    )
}

/// `nft add table ip {TABLE} 2>/dev/null || true` before the file's own leading `delete table`
/// line matters here specifically for a cold boot, when the table doesn't exist yet and `delete`
/// alone would fail -- mirrors `commands/network/setup.rs::render_service_nat_restore`, the same
/// pattern for the analogous per-project VIP-mapping restore script.
fn render_restore_script() -> String {
    format!(
        "#!/bin/sh\nset -eu\nnft add table ip {TABLE} 2>/dev/null || true\n\
         nft --check --file {RULES_PATH}\nnft --file {RULES_PATH}\n"
    )
}

fn render_unit() -> String {
    format!(
        "[Unit]\n\
         Description=Restore jiji kamal-proxy public ingress DNAT\n\
         After=docker.service network-online.target\n\
         Wants=network-online.target\n\
         ConditionPathExists={RULES_PATH}\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         ExecStart={RESTORE_SCRIPT_PATH}\n\
         RemainAfterExit=yes\n\
         \n\
         [Install]\n\
         WantedBy=multi-user.target\n"
    )
}

/// Idempotent: safe to call on every `ensure_proxy`, from any project sharing this host. Writes
/// the ruleset, restore script, and boot-persistence unit, applies it immediately (via the same
/// restore script a reboot would run), and enables the unit so a reboot restores it (nftables
/// rules don't otherwise survive one).
pub async fn ensure_ingress_rule(session: &SshSession, address: Ipv4Addr) -> anyhow::Result<()> {
    write_remote_file(
        session,
        &format!("mkdir -p {RULES_DIR}"),
        "0644",
        RULES_PATH,
        &render_nftables(address),
    )
    .await?;
    write_remote_file(
        session,
        "true",
        "0750",
        RESTORE_SCRIPT_PATH,
        &render_restore_script(),
    )
    .await?;
    write_remote_file(session, "true", "0644", UNIT_PATH, &render_unit()).await?;

    let command = format!(
        "set -eu; {RESTORE_SCRIPT_PATH}; \
         systemctl daemon-reload; systemctl enable --now {UNIT_NAME} >/dev/null"
    );
    run_required(
        session,
        &command,
        "apply the kamal-proxy public ingress rule",
    )
    .await
}

/// Used only when kamal-proxy's own container is removed (no project has routes left) --
/// tolerates the rule already being absent.
pub async fn remove_ingress_rule(session: &SshSession) -> anyhow::Result<()> {
    let command = format!(
        "systemctl disable --now {UNIT_NAME} >/dev/null 2>&1 || true; \
         rm -f {UNIT_PATH}; systemctl daemon-reload; \
         nft delete table ip {TABLE} 2>/dev/null || true; rm -rf {RULES_DIR}"
    );
    run_required(
        session,
        &command,
        "remove the kamal-proxy public ingress rule",
    )
    .await
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nftables_dnats_both_public_ports_to_the_given_address_in_both_chains() {
        let rendered = render_nftables("100.107.192.4".parse().unwrap());
        assert!(rendered.starts_with(&format!("delete table ip {TABLE}\n")));
        assert_eq!(
            rendered
                .matches("tcp dport 80 dnat to 100.107.192.4:8080")
                .count(),
            2
        );
        assert_eq!(
            rendered
                .matches("tcp dport 443 dnat to 100.107.192.4:8443")
                .count(),
            2
        );
        assert!(rendered.contains("chain prerouting"));
        assert!(rendered.contains("chain output"));
    }

    #[test]
    fn unit_runs_the_restore_script_and_survives_reboot() {
        let unit = render_unit();
        assert!(unit.contains(&format!("ExecStart={RESTORE_SCRIPT_PATH}")));
        assert!(unit.contains("WantedBy=multi-user.target"));
        assert!(unit.contains(&format!("ConditionPathExists={RULES_PATH}")));
    }

    #[test]
    fn restore_script_pre_creates_the_table_before_deleting_it() {
        // A cold boot starts with no nftables state at all -- the rules file's own leading
        // `delete table` line would fail without this, exactly as reproduced live.
        let script = render_restore_script();
        let add_pos = script.find(&format!("nft add table ip {TABLE}")).unwrap();
        let apply_pos = script.find(&format!("nft --file {RULES_PATH}")).unwrap();
        assert!(add_pos < apply_pos);
        assert!(script.starts_with("#!/bin/sh\nset -eu\n"));
    }
}
