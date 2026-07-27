use std::net::Ipv4Addr;

use jiji_config::{ContainerEngine, HealthcheckConfig, ProxyConfig, SslValue};
use jiji_network::{BackendSlot, ServiceEndpointPlan};
use jiji_ssh::SshSession;

use crate::container_runtime::backend_address;

pub struct RouteTarget {
    pub route_name: String,
    pub address: Ipv4Addr,
    pub port: u32,
    pub hosts: Vec<String>,
    pub tls: bool,
    pub path_prefix: Option<String>,
    pub healthcheck: Option<HealthcheckConfig>,
}

fn is_tls(ssl: &Option<SslValue>) -> bool {
    matches!(
        ssl,
        Some(SslValue::Enabled(true)) | Some(SslValue::Certs { .. })
    )
}

/// Normalizes `ProxyConfig`'s multi-target (`targets:`) or flat single-target fields into
/// `RouteTarget`s, resolving each target's address directly from the planned backend slot -- no
/// "query the container's live IP" step is needed, since jiji addresses are deterministic upfront.
/// An empty result means the service has no proxy configured.
pub fn targets_for_service(
    project: &str,
    service_name: &str,
    proxy: Option<&ProxyConfig>,
    endpoint: &ServiceEndpointPlan,
    slot: BackendSlot,
) -> Vec<RouteTarget> {
    let Some(proxy) = proxy else {
        return Vec::new();
    };
    let address = backend_address(endpoint, slot);

    if let Some(targets) = &proxy.targets {
        return targets
            .iter()
            .map(|target| RouteTarget {
                route_name: format!("{project}-{service_name}-{}", target.port),
                address,
                port: target.port,
                hosts: target.hosts.clone().unwrap_or_default(),
                tls: is_tls(&target.ssl),
                path_prefix: target.path_prefix.clone(),
                healthcheck: target.healthcheck.clone(),
            })
            .collect();
    }

    let Some(port) = proxy.port else {
        return Vec::new();
    };
    vec![RouteTarget {
        route_name: format!("{project}-{service_name}-{port}"),
        address,
        port,
        hosts: proxy.hosts.clone().unwrap_or_default(),
        tls: is_tls(&proxy.ssl),
        path_prefix: proxy.path_prefix.clone(),
        healthcheck: proxy.healthcheck.clone(),
    }]
}

pub fn render_deploy_command(engine: ContainerEngine, target: &RouteTarget) -> String {
    let mut args = vec![format!("--target={}:{}", target.address, target.port)];
    for host in &target.hosts {
        args.push(format!("--host={host}"));
    }
    if let Some(prefix) = &target.path_prefix {
        args.push(format!("--path-prefix={prefix}"));
    }
    if target.tls {
        args.push("--tls".to_string());
    }
    if let Some(check) = &target.healthcheck {
        if let Some(cmd) = &check.cmd {
            let cmd_value = if cmd.contains(' ') {
                format!("\"{cmd}\"")
            } else {
                cmd.clone()
            };
            args.push(format!("--health-check-cmd={cmd_value}"));
            let runtime = check.cmd_runtime.unwrap_or(engine);
            args.push(format!("--health-check-cmd-runtime={runtime}"));
        } else if let Some(path) = &check.path {
            args.push(format!("--health-check-path={path}"));
        }
        if let Some(interval) = &check.interval {
            args.push(format!("--health-check-interval={interval}"));
        }
        if let Some(timeout) = &check.timeout {
            args.push(format!("--health-check-timeout={timeout}"));
        }
        if let Some(deploy_timeout) = &check.deploy_timeout {
            args.push(format!("--deploy-timeout={deploy_timeout}"));
        }
    }
    format!(
        "{engine} exec kamal-proxy kamal-proxy deploy {} {}",
        target.route_name,
        args.join(" ")
    )
}

pub fn render_remove_command(engine: ContainerEngine, route_name: &str) -> String {
    format!("{engine} exec kamal-proxy kamal-proxy remove {route_name}")
}

pub fn render_list_command(engine: ContainerEngine) -> String {
    format!("{engine} exec kamal-proxy kamal-proxy list")
}

pub async fn deploy_route(
    session: &SshSession,
    engine: ContainerEngine,
    target: &RouteTarget,
) -> anyhow::Result<()> {
    let command = render_deploy_command(engine, target);
    let result = session.execute(&command).await?;
    if !result.success {
        anyhow::bail!(
            "Could not deploy proxy route '{}' on {}: {}",
            target.route_name,
            session.host(),
            result.stderr.trim()
        );
    }
    Ok(())
}

pub async fn remove_route(
    session: &SshSession,
    engine: ContainerEngine,
    route_name: &str,
) -> anyhow::Result<()> {
    let command = render_remove_command(engine, route_name);
    let result = session.execute(&command).await?;
    if !result.success {
        anyhow::bail!(
            "Could not remove proxy route '{route_name}' on {}: {}",
            session.host(),
            result.stderr.trim()
        );
    }
    Ok(())
}

pub async fn verify_route(
    session: &SshSession,
    engine: ContainerEngine,
    route_name: &str,
) -> anyhow::Result<()> {
    let command = render_list_command(engine);
    let result = session.execute(&command).await?;
    if !result.success || !result.stdout.contains(route_name) {
        anyhow::bail!(
            "Proxy route '{route_name}' is not listed by kamal-proxy on {}. Inspect it with `{engine} exec kamal-proxy kamal-proxy list`.",
            session.host()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiji_network::NetworkPlanner;

    fn endpoint() -> (ServiceEndpointPlan, BackendSlot) {
        let config: jiji_config::Config = serde_yaml::from_str(
            r#"
project: demo
builder: { engine: docker }
servers:
  app: { host: 203.0.113.10 }
services:
  web: { image: example/web, servers: [app] }
"#,
        )
        .unwrap();
        let plan = NetworkPlanner::new().plan(&config).unwrap();
        (plan.endpoints["demo:web:app"].clone(), BackendSlot::A)
    }

    #[test]
    fn single_target_flat_config_produces_one_route() {
        let (endpoint, slot) = endpoint();
        let proxy: ProxyConfig =
            serde_yaml::from_str("port: 3000\nhosts: [example.com]\nssl: true\n").unwrap();
        let targets = targets_for_service("demo", "web", Some(&proxy), &endpoint, slot);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].route_name, "demo-web-3000");
        assert_eq!(targets[0].hosts, vec!["example.com".to_string()]);
        assert!(targets[0].tls);
    }

    #[test]
    fn multi_target_config_produces_one_route_per_target() {
        let (endpoint, slot) = endpoint();
        let proxy: ProxyConfig = serde_yaml::from_str(
            r#"
targets:
  - port: 3900
    hosts: [s3.example.com]
  - port: 3903
    hosts: [admin.example.com]
    ssl: true
"#,
        )
        .unwrap();
        let targets = targets_for_service("demo", "web", Some(&proxy), &endpoint, slot);
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].route_name, "demo-web-3900");
        assert_eq!(targets[1].route_name, "demo-web-3903");
        assert!(targets[1].tls);
    }

    #[test]
    fn no_proxy_config_means_no_routes() {
        let (endpoint, slot) = endpoint();
        assert!(targets_for_service("demo", "web", None, &endpoint, slot).is_empty());
    }

    #[test]
    fn deploy_command_renders_http_healthcheck() {
        let (endpoint, slot) = endpoint();
        let proxy: ProxyConfig = serde_yaml::from_str(
            "port: 3000\nhosts: [example.com]\nhealthcheck: { path: /health, interval: 10s, deploy_timeout: 60s }\n",
        )
        .unwrap();
        let target = &targets_for_service("demo", "web", Some(&proxy), &endpoint, slot)[0];
        let command = render_deploy_command(ContainerEngine::Docker, target);
        assert!(command.starts_with("docker exec kamal-proxy kamal-proxy deploy demo-web-3000"));
        assert!(command.contains("--target="));
        assert!(command.contains("--host=example.com"));
        assert!(command.contains("--health-check-path=/health"));
        assert!(command.contains("--health-check-interval=10s"));
        assert!(command.contains("--deploy-timeout=60s"));
        assert!(!command.contains("--health-check-cmd"));
    }

    #[test]
    fn deploy_command_renders_command_healthcheck_with_runtime() {
        let (endpoint, slot) = endpoint();
        let proxy: ProxyConfig = serde_yaml::from_str(
            "port: 3000\nhosts: [example.com]\nhealthcheck: { cmd: \"test -f /ready\" }\n",
        )
        .unwrap();
        let target = &targets_for_service("demo", "web", Some(&proxy), &endpoint, slot)[0];
        let command = render_deploy_command(ContainerEngine::Podman, target);
        assert!(command.contains("--health-check-cmd=\"test -f /ready\""));
        assert!(command.contains("--health-check-cmd-runtime=podman"));
    }

    #[test]
    fn path_prefix_and_tls_are_rendered_when_set() {
        let (endpoint, slot) = endpoint();
        let proxy: ProxyConfig = serde_yaml::from_str(
            "port: 3000\nhosts: [example.com]\npath_prefix: /api\nssl: true\n",
        )
        .unwrap();
        let target = &targets_for_service("demo", "web", Some(&proxy), &endpoint, slot)[0];
        let command = render_deploy_command(ContainerEngine::Docker, target);
        assert!(command.contains("--path-prefix=/api"));
        assert!(command.contains("--tls"));
    }
}
