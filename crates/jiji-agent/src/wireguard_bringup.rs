//! Native equivalent of restarting `wg-quick@{interface}.service`: brings up the WireGuard kernel
//! interface directly (`ip link add ... type wireguard`, `wg set ... private-key ... listen-port
//! ...`, address assignment, link up), replicating the parts of a `wg-quick up` that jiji's own
//! rendered config actually uses -- jiji never sets `PostUp`/`PostDown`/`DNS`/`Table`/`MTU` in the
//! `[Interface]` section it generates (`commands/network/setup.rs::render_wireguard`), so those
//! `wg-quick` features have nothing to replicate here. Peer configuration is unrelated to this
//! module: it is applied incrementally by `wireguard.rs`'s `plan_reconciliation`/`render_commands`
//! once membership has replicated, exactly as it already was before Phase 9.
//!
//! The private key value itself is never read into this process' memory as a config field --
//! `wg set`'s own `private-key <path>` option reads the file directly, so only the path
//! (`MeshConfig::wireguard_private_key_path`) ever needs to flow through configuration.

use std::net::Ipv4Addr;
use std::path::Path;

use tokio::process::Command;

pub async fn bring_up_interface(
    interface: &str,
    management_address: Ipv4Addr,
    listen_port: u16,
    private_key_path: &Path,
) -> Result<(), String> {
    if !command_ok("ip", &["link", "show", "dev", interface]).await {
        run("ip", &["link", "add", interface, "type", "wireguard"]).await?;
    }
    let private_key_path = private_key_path
        .to_str()
        .ok_or_else(|| "WireGuard private key path is not valid UTF-8".to_string())?;
    run(
        "wg",
        &[
            "set",
            interface,
            "private-key",
            private_key_path,
            "listen-port",
            &listen_port.to_string(),
        ],
    )
    .await?;
    run(
        "ip",
        &[
            "address",
            "replace",
            &format!("{management_address}/32"),
            "dev",
            interface,
        ],
    )
    .await?;
    run("ip", &["link", "set", interface, "up"]).await
}

async fn command_ok(binary: &str, args: &[&str]) -> bool {
    Command::new(binary)
        .args(args)
        .output()
        .await
        .is_ok_and(|output| output.status.success())
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
