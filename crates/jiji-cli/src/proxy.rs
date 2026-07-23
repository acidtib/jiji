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
const INTERNAL_HTTP_PORT: u16 = 8080;
const INTERNAL_HTTPS_PORT: u16 = 8443;

pub enum ProxyStatus {
    AlreadyRunning,
    Started,
}

/// The private-network addresses kamal-proxy needs. Always both-or-neither: absent when
/// `network.enabled` is false, present (and pinned) otherwise.
#[derive(Debug, Clone, Copy)]
pub struct ProxyNetwork {
    pub dns_address: Ipv4Addr,
    pub proxy_address: Ipv4Addr,
}

pub async fn ensure_proxy(
    session: &SshSession,
    engine: ContainerEngine,
    network: Option<ProxyNetwork>,
) -> anyhow::Result<ProxyStatus> {
    ensure_network(session, engine).await?;

    let fingerprint = config_fingerprint(engine, network);
    if is_current_and_running(session, engine, &fingerprint).await? {
        return Ok(ProxyStatus::AlreadyRunning);
    }

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
            "Could not replace kamal-proxy on {}: {}. Remove the existing '{}' container and retry `jiji server setup`.",
            session.host(),
            remove.stderr.trim(),
            CONTAINER_NAME
        );
    }

    let command = run_command(engine, network, &fingerprint);
    run_required(session, &command, "start kamal-proxy").await?;
    wait_until_running(session, engine).await?;

    Ok(ProxyStatus::Started)
}

async fn ensure_network(session: &SshSession, engine: ContainerEngine) -> anyhow::Result<()> {
    let inspect = session
        .execute(match engine {
            ContainerEngine::Docker => "docker network inspect jiji",
            ContainerEngine::Podman => "podman network inspect jiji",
        })
        .await?;
    if inspect.success {
        return Ok(());
    }

    run_required(
        session,
        &format!("{engine} network create jiji"),
        "create the jiji container network",
    )
    .await
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
    network: Option<ProxyNetwork>,
    fingerprint: &str,
) -> String {
    let network_args = network.map_or_else(String::new, |network| {
        format!(
            " --ip {} --dns {} --dns-search jiji --dns-option ndots:1",
            network.proxy_address, network.dns_address
        )
    });
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

    format!(
        "{engine} run --name {CONTAINER_NAME} --network jiji{network_args} --detach \
         --restart unless-stopped --label jiji.managed=true \
         --label jiji.proxy-config={fingerprint} \
         --volume {CONFIG_VOLUME}:/home/kamal-proxy/.config/kamal-proxy \
         --volume {CERTS_DIR}:/jiji-certs:ro{runtime} \
         --publish 80:{INTERNAL_HTTP_PORT} --publish 443:{INTERNAL_HTTPS_PORT} \
         {IMAGE} kamal-proxy run --http-port {INTERNAL_HTTP_PORT} \
         --https-port {INTERNAL_HTTPS_PORT}"
    )
}

fn config_fingerprint(engine: ContainerEngine, network: Option<ProxyNetwork>) -> String {
    format!(
        "v2-{engine}-{}",
        network.map_or_else(
            || "none".to_string(),
            |network| format!("{}-{}", network.proxy_address, network.dns_address)
        )
    )
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
        "kamal-proxy did not become ready on {} within 30 seconds. Inspect it with `{engine} logs {CONTAINER_NAME}` and retry `jiji server setup`. Recent logs: {}",
        session.host(),
        logs.stdout.trim()
    )
}

async fn run_required(session: &SshSession, command: &str, action: &str) -> anyhow::Result<()> {
    let result = session.execute(command).await?;
    if !result.success {
        anyhow::bail!(
            "Could not {action} on {}: {}. Fix the host error and retry `jiji server setup`.",
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

#[cfg(test)]
mod tests {
    use super::*;

    fn network(proxy_address: &str, dns_address: &str) -> ProxyNetwork {
        ProxyNetwork {
            proxy_address: proxy_address.parse().unwrap(),
            dns_address: dns_address.parse().unwrap(),
        }
    }

    #[test]
    fn docker_run_pins_the_proxy_address_and_uses_local_dns() {
        let command = run_command(
            ContainerEngine::Docker,
            Some(network("10.0.16.3", "10.0.16.2")),
            "v2-docker-10.0.16.3-10.0.16.2",
        );

        assert!(command.contains("ghcr.io/acidtib/kamal-proxy:jiji"));
        assert!(command.contains("--network jiji --ip 10.0.16.3 --dns 10.0.16.2"));
        assert!(command.contains("--publish 80:8080 --publish 443:8443"));
        assert!(command.contains("/var/run/docker.sock:/var/run/docker.sock"));
        assert!(command.contains("--restart unless-stopped"));
    }

    #[test]
    fn no_network_omits_ip_and_dns_flags() {
        let command = run_command(ContainerEngine::Docker, None, "v2-docker-none");
        assert!(command.contains("--network jiji --detach"));
        assert!(!command.contains("--ip"));
        assert!(!command.contains("--dns"));
    }

    #[test]
    fn podman_run_has_command_health_check_access() {
        let command = run_command(
            ContainerEngine::Podman,
            Some(network("10.0.32.3", "10.0.32.2")),
            "v2-podman-10.0.32.3-10.0.32.2",
        );

        assert!(command.contains("--privileged --user root --pid=host --cgroupns=host"));
        assert!(command.contains("/var/lib/containers:/var/lib/containers"));
        assert!(!command.contains("docker.sock"));
    }
}
