use std::net::Ipv4Addr;
use std::time::Duration;

use jiji_config::ContainerEngine;
use jiji_ssh::SshSession;

pub(crate) const CONTAINER_NAME: &str = "kamal-proxy";
const IMAGE: &str = "ghcr.io/acidtib/kamal-proxy:jiji";
// `pub(crate)`: reused by `crate::proxy_teardown` to clean these up when kamal-proxy itself is
// torn down (no project needs it anymore).
pub(crate) const CONFIG_VOLUME: &str = "kamal-proxy-config";
pub(crate) const CERTS_DIR: &str = "/etc/jiji/certs";
// `pub(crate)`: `proxy_ingress` needs the same ports for its Docker-only nftables DNAT workaround.
pub(crate) const INTERNAL_HTTP_PORT: u16 = 8080;
pub(crate) const INTERNAL_HTTPS_PORT: u16 = 8443;

pub enum ProxyStatus {
    AlreadyRunning,
    Started,
}

/// The private-network address kamal-proxy needs on *this project's* bridge. Absent when
/// `network.enabled` is false, present (and pinned) otherwise. kamal-proxy is a single, shared,
/// per-host container that becomes multi-homed across every project's bridge that has active
/// routes on that host -- see `ensure_proxy`'s doc comment.
#[derive(Debug, Clone)]
pub struct ProxyNetwork {
    pub bridge_name: String,
    pub proxy_address: Ipv4Addr,
}

/// Ensures kamal-proxy is running with the current image/engine, and (if `network` is given)
/// attached to this project's bridge at its pinned `proxy_address`.
///
/// kamal-proxy is the one deliberately shared, multi-tenant, per-host component in jiji's
/// otherwise per-project-isolated network design (see the project's network-isolation notes): one
/// container, routes namespaced per project, serving every project on a host at once. Under that
/// design it must become **multi-homed** -- attached to every project's bridge that has routes on
/// this host, not just the most recent one. That means "does the container need replacing"
/// (image/engine drift, decided by `config_fingerprint`/`is_current_and_running`) and "is this
/// project's network attached" (`ensure_attached`, additive, idempotent) have to be two
/// independent steps: recreating the container just because a *different* project's `ensure_proxy`
/// call ran would tear down every other project's attachment too.
///
/// kamal-proxy is deliberately given no `--dns`/`--dns-search` here (unlike a project's own
/// service containers): its routing targets are raw backend IPs
/// (`proxy_routes::RouteTarget::address`, never a `.jiji` hostname), so it has no need to resolve
/// private DNS at all. This also sidesteps a real multi-homing problem: a single resolv.conf
/// pointed at multiple projects' dnsmasq instances wouldn't reliably resolve names across all of
/// them (a resolver conventionally stops at the first definitive NXDOMAIN rather than falling
/// through to the next nameserver), and per-project DNS can't be changed after container creation
/// via `network connect` anyway.
pub async fn ensure_proxy(
    session: &SshSession,
    engine: ContainerEngine,
    network: Option<ProxyNetwork>,
    force: bool,
) -> anyhow::Result<ProxyStatus> {
    let fingerprint = config_fingerprint(engine);
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
        // Docker only: see `proxy_ingress` for why `--publish` alone isn't enough on jiji's
        // bridge networks. Re-applied on every call, from any project sharing this host, so it
        // self-heals and always targets a currently-attached address.
        if engine == ContainerEngine::Docker {
            crate::proxy_ingress::ensure_ingress_rule(session, network.proxy_address).await?;
        }
    }

    Ok(status)
}

async fn recreate(
    session: &SshSession,
    engine: ContainerEngine,
    fingerprint: &str,
    network: Option<&ProxyNetwork>,
) -> anyhow::Result<()> {
    run_required(
        session,
        &format!("mkdir -p {CERTS_DIR}"),
        "create certificate directory",
    )
    .await?;
    run_required(
        session,
        &format!("{engine} pull {IMAGE}"),
        "pull kamal-proxy image",
    )
    .await?;

    let remove = session
        .execute(&format!("{engine} container rm -f {CONTAINER_NAME}"))
        .await?;
    if !remove.success && !is_missing_container_error(&remove.stderr) {
        anyhow::bail!(
            "Could not replace kamal-proxy on {}: {}. Remove the existing '{}' container and retry the command.",
            session.host(),
            remove.stderr.trim(),
            CONTAINER_NAME
        );
    }

    let command = run_command(engine, network, fingerprint);
    run_required(session, &command, "start kamal-proxy").await?;
    wait_until_running(session, engine).await
}

/// Idempotently attaches kamal-proxy to `network.bridge_name` at `network.proxy_address`, additive
/// only -- never touches any other network kamal-proxy might already be attached to for other
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
            "Could not inspect kamal-proxy's attached networks on {}: {}",
            session.host(),
            result.stderr.trim()
        );
    }

    if let Some(existing) = attached_address(&result.stdout, &network.bridge_name) {
        if existing == network.proxy_address {
            return Ok(());
        }
        anyhow::bail!(
            "kamal-proxy on {} is already attached to network '{}' with address {existing}, expected {}. Remove the container with `{engine} rm -f {CONTAINER_NAME}` and retry, or investigate the address drift.",
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
            "Could not attach kamal-proxy to network '{}' on {}: {}. Run `jiji network setup` for this project and retry.",
            network.bridge_name,
            session.host(),
            result.stderr.trim()
        );
    }
    Ok(())
}

/// Parses `{{json .NetworkSettings.Networks}}` output (an object keyed by network name, each value
/// carrying at least an `IPAddress` field) and returns the address attached to `bridge_name`, if
/// any. A separate, pure function so the parsing logic is unit-testable without a live container
/// engine.
fn attached_address(networks_json: &str, bridge_name: &str) -> Option<Ipv4Addr> {
    let value: serde_json::Value = serde_json::from_str(networks_json.trim()).ok()?;
    let address = value.get(bridge_name)?.get("IPAddress")?.as_str()?;
    if address.is_empty() {
        return None;
    }
    address.parse().ok()
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
    Ok(result.success && result.stdout.trim() == format!("running {fingerprint} {IMAGE}"))
}

fn run_command(
    engine: ContainerEngine,
    network: Option<&ProxyNetwork>,
    fingerprint: &str,
) -> String {
    let runtime = match engine {
        ContainerEngine::Docker => {
            " --volume /var/run/docker.sock:/var/run/docker.sock".to_string()
        }
        ContainerEngine::Podman => concat!(
            " --privileged --user root --pid=host --cgroupns=host",
            " --volume /run:/run",
            " --volume /usr/bin:/usr/bin:ro",
            " --volume /usr/lib:/usr/lib:ro",
            " --volume /lib:/lib:ro",
            " --volume /lib64:/lib64:ro",
            " --volume /var/lib/containers:/var/lib/containers"
        )
        .to_string(),
    };

    // Confirmed live: Docker (and Podman) refuse to `network connect` *anything* to a container
    // created with `--network none` -- "none" is an exclusive private mode, not an empty set of
    // attachments. So the network that triggered this (re)creation must be attached right here,
    // as the primary network; every other project's bridge is added afterward via `ensure_attached`
    // (`network connect`), which works fine once there's at least one real network already. A
    // project with `network.enabled: false` still falls back to `--network none` -- if that
    // project is ever the one that (re)creates kamal-proxy on a host shared with network-enabled
    // projects, those projects' `network connect` calls will fail until a network-enabled
    // project's own `ensure_proxy` recreates the container; documented as a known limitation
    // rather than solved here, since disabling private networking is an explicit opt-out.
    let network_args = network.map_or_else(
        || " --network none".to_string(),
        |network| {
            format!(
                " --network {} --ip {}",
                network.bridge_name, network.proxy_address
            )
        },
    );

    format!(
        "{engine} run --name {CONTAINER_NAME}{network_args} --detach \
         --restart unless-stopped --label jiji.managed=true \
         --label jiji.proxy-config={fingerprint} \
         --volume {CONFIG_VOLUME}:/home/kamal-proxy/.config/kamal-proxy \
         --volume {CERTS_DIR}:/jiji-certs:ro{runtime} \
         --publish 80:{INTERNAL_HTTP_PORT} --publish 443:{INTERNAL_HTTPS_PORT} \
         {IMAGE} kamal-proxy run --http-port {INTERNAL_HTTP_PORT} \
         --https-port {INTERNAL_HTTPS_PORT}"
    )
}

/// Identity used purely to decide "does the running container need replacing" (image/engine
/// drift) -- deliberately excludes any project's network address, since kamal-proxy can be
/// attached to several projects' bridges at once and none of them singularly identifies "the"
/// container's configuration anymore. Bumped to v3 (from v2, which embedded one project's
/// proxy/dns address) as part of the multi-homing change.
fn config_fingerprint(engine: ContainerEngine) -> String {
    format!("v3-{engine}")
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
        "kamal-proxy did not become ready on {} within 30 seconds. Inspect it with `{engine} logs {CONTAINER_NAME}` and retry the command. Recent logs: {}",
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

fn is_missing_container_error(stderr: &str) -> bool {
    let stderr = stderr.to_ascii_lowercase();
    stderr.contains("no such container") || stderr.contains("no container with name or id")
}

/// Disconnects kamal-proxy from `bridge_name` if it's attached, tolerating "kamal-proxy doesn't
/// exist" and "not attached to this network" as success (returning `false`, not an error). Used
/// by `commands/server/teardown.rs` before removing a project's bridge network -- kamal-proxy may
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
    if is_missing_container_error(&result.stderr) {
        return Ok(false);
    }
    let stderr = result.stderr.to_ascii_lowercase();
    if stderr.contains("is not connected") || stderr.contains("not found") {
        return Ok(false);
    }
    anyhow::bail!(
        "Could not disconnect kamal-proxy from network '{bridge_name}' on {}: {}",
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
            proxy_address: proxy_address.parse().unwrap(),
        }
    }

    #[test]
    fn docker_run_omits_network_when_none_given_and_never_sets_dns() {
        let command = run_command(ContainerEngine::Docker, None, "v3-docker");

        assert!(command.contains("ghcr.io/acidtib/kamal-proxy:jiji"));
        assert!(command.contains("--network none --detach"));
        assert!(!command.contains("--ip"));
        assert!(!command.contains("--dns"));
        assert!(command.contains("--publish 80:8080 --publish 443:8443"));
        assert!(command.contains("/var/run/docker.sock:/var/run/docker.sock"));
        assert!(command.contains("--restart unless-stopped"));
    }

    #[test]
    fn docker_run_attaches_the_given_network_as_primary_at_creation() {
        // `--network none` is a Docker/Podman-exclusive private mode: a container created with it
        // can never have a real network `connect`ed afterward (confirmed live). So whichever
        // project's `ensure_proxy` call (re)creates the container must attach *its* network right
        // here, not rely purely on a later `network connect`.
        let net = network("jiji-demo-9f8e7d6c", "10.0.2.9");
        let command = run_command(ContainerEngine::Docker, Some(&net), "v3-docker");

        assert!(command.contains("--network jiji-demo-9f8e7d6c --ip 10.0.2.9 --detach"));
        assert!(!command.contains("--network none"));
        assert!(!command.contains("--dns"));
    }

    #[test]
    fn podman_run_has_command_health_check_access() {
        let command = run_command(ContainerEngine::Podman, None, "v3-podman");

        assert!(command.contains("--privileged --user root --pid=host --cgroupns=host"));
        assert!(command.contains("/var/lib/containers:/var/lib/containers"));
        assert!(!command.contains("docker.sock"));
    }

    #[test]
    fn attached_address_finds_the_named_network_and_ignores_others() {
        let json = r#"{"jiji-other-1a2b3c4d":{"IPAddress":"10.0.1.5"},"jiji-demo-9f8e7d6c":{"IPAddress":"10.0.2.9"}}"#;
        assert_eq!(
            attached_address(json, "jiji-demo-9f8e7d6c"),
            Some("10.0.2.9".parse().unwrap())
        );
        assert_eq!(attached_address(json, "jiji-missing"), None);
    }

    #[test]
    fn attached_address_handles_none_and_empty_address() {
        assert_eq!(attached_address("{}", "jiji-demo"), None);
        assert_eq!(
            attached_address(r#"{"jiji-demo":{"IPAddress":""}}"#, "jiji-demo"),
            None
        );
        assert_eq!(attached_address("null", "jiji-demo"), None);
        assert_eq!(attached_address("not json", "jiji-demo"), None);
    }

    #[test]
    fn network_test_helper_still_builds_the_expected_struct() {
        let net = network("jiji-demo-9f8e7d6c", "10.0.2.9");
        assert_eq!(net.bridge_name, "jiji-demo-9f8e7d6c");
        assert_eq!(net.proxy_address, "10.0.2.9".parse::<Ipv4Addr>().unwrap());
    }
}
