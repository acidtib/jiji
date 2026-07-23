use std::net::Ipv4Addr;
use std::time::Duration;

use jiji_config::ContainerEngine;
use jiji_ssh::SshSession;

const CONTAINER_NAME: &str = "kamal-proxy";
const IMAGE: &str = "ghcr.io/acidtib/kamal-proxy:jiji";
const CONFIG_VOLUME: &str = "kamal-proxy-config";
const INTERNAL_HTTP_PORT: u16 = 8080;
const INTERNAL_HTTPS_PORT: u16 = 8443;

pub enum ProxyStatus {
    AlreadyRunning,
    Started,
}

pub async fn ensure_proxy(
    session: &SshSession,
    engine: ContainerEngine,
    dns_address: Option<Ipv4Addr>,
) -> anyhow::Result<ProxyStatus> {
    ensure_network(session, engine).await?;

    let fingerprint = config_fingerprint(engine, dns_address);
    if is_current_and_running(session, engine, &fingerprint).await? {
        return Ok(ProxyStatus::AlreadyRunning);
    }

    run_required(
        session,
        "mkdir -p /etc/jiji/certs",
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

    let command = run_command(engine, dns_address, &fingerprint);
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
    dns_address: Option<Ipv4Addr>,
    fingerprint: &str,
) -> String {
    let dns = dns_address.map_or_else(String::new, |address| {
        format!(" --dns {address} --dns-search jiji --dns-option ndots:1")
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
        "{engine} run --name {CONTAINER_NAME} --network jiji{dns} --detach \
         --restart unless-stopped --label jiji.managed=true \
         --label jiji.proxy-config={fingerprint} \
         --volume {CONFIG_VOLUME}:/home/kamal-proxy/.config/kamal-proxy \
         --volume /etc/jiji/certs:/jiji-certs:ro{runtime} \
         --publish 80:{INTERNAL_HTTP_PORT} --publish 443:{INTERNAL_HTTPS_PORT} \
         {IMAGE} kamal-proxy run --http-port {INTERNAL_HTTP_PORT} \
         --https-port {INTERNAL_HTTPS_PORT}"
    )
}

fn config_fingerprint(engine: ContainerEngine, dns_address: Option<Ipv4Addr>) -> String {
    format!(
        "v1-{engine}-{}",
        dns_address.map_or_else(|| "none".to_string(), |address| address.to_string())
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

    #[test]
    fn docker_run_uses_the_fork_and_local_dns() {
        let command = run_command(
            ContainerEngine::Docker,
            Some("10.0.16.2".parse().unwrap()),
            "v1-docker-10.0.16.2",
        );

        assert!(command.contains("ghcr.io/acidtib/kamal-proxy:jiji"));
        assert!(command.contains("--network jiji --dns 10.0.16.2"));
        assert!(command.contains("--publish 80:8080 --publish 443:8443"));
        assert!(command.contains("/var/run/docker.sock:/var/run/docker.sock"));
        assert!(command.contains("--restart unless-stopped"));
    }

    #[test]
    fn podman_run_has_command_health_check_access() {
        let command = run_command(
            ContainerEngine::Podman,
            Some("10.0.32.2".parse().unwrap()),
            "v1-podman-10.0.32.2",
        );

        assert!(command.contains("--privileged --user root --pid=host --cgroupns=host"));
        assert!(command.contains("/var/lib/containers:/var/lib/containers"));
        assert!(!command.contains("docker.sock"));
    }
}
