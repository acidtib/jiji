use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;
use std::sync::Arc;

use jiji_config::{ContainerEngine, HealthcheckConfig, ProxyConfig, SslValue};
use jiji_ssh::SshSession;

use crate::container_runtime::exec_prefix;
use crate::health_check;

const DEFAULT_PROXY_DEPLOY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const DEFAULT_PROXY_HEALTH_TIMEOUT: &str = "5s";
const PROXY_DEPLOY_TIMEOUT_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

pub struct RouteTarget {
    pub route_name: String,
    pub address: Ipv4Addr,
    /// Additional healthy replicas admitted to the same kamal-proxy route.
    pub additional_addresses: Vec<Ipv4Addr>,
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

pub fn targets_for_address(
    project: &str,
    service_name: &str,
    proxy: Option<&ProxyConfig>,
    address: Ipv4Addr,
) -> Vec<RouteTarget> {
    let Some(proxy) = proxy else {
        return Vec::new();
    };
    if let Some(targets) = &proxy.targets {
        return targets
            .iter()
            .map(|target| RouteTarget {
                route_name: format!("{project}-{service_name}-{}", target.port),
                address,
                additional_addresses: Vec::new(),
                port: target.port,
                hosts: target.hosts.clone().unwrap_or_default(),
                tls: is_tls(&target.ssl),
                path_prefix: target.path_prefix.clone(),
                healthcheck: target.healthcheck.clone(),
            })
            .collect();
    }
    proxy
        .port
        .map(|port| RouteTarget {
            route_name: format!("{project}-{service_name}-{port}"),
            address,
            additional_addresses: Vec::new(),
            port,
            hosts: proxy.hosts.clone().unwrap_or_default(),
            tls: is_tls(&proxy.ssl),
            path_prefix: proxy.path_prefix.clone(),
            healthcheck: proxy.healthcheck.clone(),
        })
        .into_iter()
        .collect()
}

pub fn render_deploy_command(engine: ContainerEngine, target: &RouteTarget) -> String {
    let mut addresses = vec![target.address];
    addresses.extend(target.additional_addresses.iter().copied());
    addresses.sort();
    addresses.dedup();
    let mut args = addresses
        .into_iter()
        .map(|address| format!("--target={address}:{}", target.port))
        .collect::<Vec<_>>();
    let (mut static_args, deploy_timeout) = render_static_deploy_args(engine, target);
    args.append(&mut static_args);
    let process_timeout = deploy_timeout.saturating_add(PROXY_DEPLOY_TIMEOUT_GRACE);
    let exec = exec_prefix(engine);
    format!(
        "timeout --signal=TERM --kill-after=5s {}s {exec} kamal-proxy kamal-proxy deploy {} {}",
        process_timeout.as_secs(),
        target.route_name,
        args.join(" ")
    )
}

fn render_static_deploy_args(
    engine: ContainerEngine,
    target: &RouteTarget,
) -> (Vec<String>, std::time::Duration) {
    let mut args = Vec::new();
    let mut deploy_timeout = DEFAULT_PROXY_DEPLOY_TIMEOUT;
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
        args.push(format!(
            "--health-check-timeout={}",
            check
                .timeout
                .as_deref()
                .unwrap_or(DEFAULT_PROXY_HEALTH_TIMEOUT)
        ));
        if let Some(configured_deploy_timeout) = &check.deploy_timeout {
            args.push(format!("--deploy-timeout={configured_deploy_timeout}"));
            if let Some(parsed) = health_check::parse_duration(configured_deploy_timeout) {
                deploy_timeout = parsed;
            }
        }
    }
    (args, deploy_timeout)
}

pub fn runtime_specs_for_service(
    engine: ContainerEngine,
    project: &str,
    service: &str,
    proxy: &ProxyConfig,
) -> Vec<jiji_agent::runtime::ProxyRouteSpec> {
    targets_for_address(project, service, Some(proxy), Ipv4Addr::UNSPECIFIED)
        .into_iter()
        .map(|target| {
            let (deploy_args, timeout) = render_static_deploy_args(engine, &target);
            jiji_agent::runtime::ProxyRouteSpec {
                service: service.to_string(),
                route_name: target.route_name,
                port: target.port,
                deploy_args,
                deploy_timeout_secs: timeout.saturating_add(PROXY_DEPLOY_TIMEOUT_GRACE).as_secs(),
            }
        })
        .collect()
}

pub fn render_remove_command(engine: ContainerEngine, route_name: &str) -> String {
    format!(
        "{} kamal-proxy kamal-proxy remove {route_name}",
        exec_prefix(engine)
    )
}

pub fn render_list_command(engine: ContainerEngine) -> String {
    format!("{} kamal-proxy kamal-proxy list", exec_prefix(engine))
}

/// Builds a best-effort host command that drops kamal-proxy's own cached neighbor (ARP) entries
/// for the given addresses inside kamal-proxy's own network namespace.
///
/// Confirmed live: a dynamically leased address is reused across deployments (the allocator draws
/// from a small per-bridge pool), and kamal-proxy's network namespace keeps its own neighbor table
/// independent of the host's. When a fresh container starts with a new MAC at a previously used
/// address, kamal-proxy's stale entry (old MAC, STALE state) sends a unicast probe to a MAC that no
/// longer exists before falling back to broadcast, which can take Linux's neighbor
/// STALE/DELAY/PROBE cycle (tens of seconds) to resolve -- surfacing as "no route to host" from
/// kamal-proxy's own dial and failing its deploy health check, even though the host's own route to
/// the same address is immediately usable. Flushing the specific entries before every deploy
/// forces an immediate fresh ARP resolution instead of waiting out the stale-entry timeout.
fn render_neighbor_refresh_command(engine: ContainerEngine, addresses: &[Ipv4Addr]) -> String {
    let flush = addresses
        .iter()
        .map(|address| format!("ip neigh flush to {address} dev eth0"))
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "pid=$({engine} inspect -f '{{{{.State.Pid}}}}' kamal-proxy 2>/dev/null); [ -n \"$pid\" ] && nsenter -t \"$pid\" -n sh -c '{flush}' 2>/dev/null; true"
    )
}

pub async fn deploy_route(
    session: &SshSession,
    engine: ContainerEngine,
    target: &RouteTarget,
) -> anyhow::Result<()> {
    let mut addresses = vec![target.address];
    addresses.extend(target.additional_addresses.iter().copied());
    // Best-effort: a failed flush (kamal-proxy not yet running, nsenter unavailable) must never
    // block the deploy itself, only forgo the stale-entry workaround.
    let _ = session
        .execute(&render_neighbor_refresh_command(engine, &addresses))
        .await;
    let command = render_deploy_command(engine, target);
    let result = session.execute(&command).await?;
    if !result.success {
        let timeout_hint = if result.code == Some(124) {
            " The proxy deployment exceeded Jiji's outer timeout and was terminated."
        } else {
            ""
        };
        anyhow::bail!(
            "Could not deploy proxy route '{}' on {}: {}{}",
            target.route_name,
            session.host(),
            result.stderr.trim(),
            timeout_hint
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
            "Proxy route '{route_name}' is not listed by kamal-proxy on {}. Inspect it with `{} kamal-proxy kamal-proxy list`.",
            session.host(),
            exec_prefix(engine)
        );
    }
    Ok(())
}

/// Rebuild every selected service route from the union of catalog views available on the
/// connected ingress agents. This makes route reconciliation independent of which replica was
/// deployed and admits remote healthy replicas in one idempotent command per route/ingress.
///
/// `fresh_addresses` (service -> replica_id -> address) overrides the catalog-derived address for
/// any replica it names. Confirmed live: a sibling ingress host's own catalog replica can lag the
/// owning host's just-committed write by up to one P2P replication interval, which briefly made a
/// concurrent multi-replica deploy build a route to a several-generations-stale, already-torn-down
/// address (kamal-proxy then failed its own health check against it with "no route to host").
/// Since a caller that just deployed a replica in this same invocation already has its true,
/// durably-committed address with no replication round trip needed, that value is always
/// authoritative over whatever the catalog read happens to currently show for the same replica.
pub async fn reconcile_catalog_routes(
    sessions: &BTreeMap<String, Arc<SshSession>>,
    project: &str,
    engine: ContainerEngine,
    services: &BTreeMap<String, ProxyConfig>,
    fresh_addresses: &BTreeMap<String, BTreeMap<String, Ipv4Addr>>,
) -> anyhow::Result<()> {
    if services.is_empty() || sessions.is_empty() {
        return Ok(());
    }
    let mut records = Vec::new();
    for session in sessions.values() {
        records.extend(crate::agent_client::catalog(session, project).await?);
    }
    for (service, proxy) in services {
        // Keep every observed signed revision. During convergence one ingress may still report an
        // older Active record while the owner already reports its newer tombstone. Collapsing the
        // union by deployment id makes iteration order decide which revision survives and can
        // re-admit a removed, unhealthy target. Winner selection must see both revisions.
        let mut by_replica: BTreeMap<String, Ipv4Addr> =
            jiji_agent::catalog::active_healthy_winners(&records)
                .into_iter()
                .filter(|record| record.service == *service)
                .map(|record| (record.replica_id.clone(), record.address))
                .collect();
        if let Some(fresh) = fresh_addresses.get(service) {
            for (replica_id, address) in fresh {
                by_replica.insert(replica_id.clone(), *address);
            }
        }
        let addresses = by_replica.into_values().collect::<BTreeSet<_>>();
        let Some(primary) = addresses.first().copied() else {
            // Scale-to-zero and final removal must withdraw stale ingress even
            // though there is no remaining backend address from which to
            // construct a deploy target. Route names depend only on project,
            // service, and configured port, so a placeholder is safe here.
            let placeholder = Ipv4Addr::UNSPECIFIED;
            for session in sessions.values() {
                for target in targets_for_address(project, service, Some(proxy), placeholder) {
                    remove_route(session, engine, &target.route_name).await?;
                }
            }
            continue;
        };
        let additional = addresses.iter().copied().skip(1).collect::<Vec<_>>();
        let mut targets = targets_for_address(project, service, Some(proxy), primary);
        for target in &mut targets {
            target.additional_addresses = additional.clone();
        }
        for session in sessions.values() {
            for target in &targets {
                deploy_route(session, engine, target).await?;
                verify_route(session, engine, &target.route_name).await?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn address() -> Ipv4Addr {
        "100.64.0.10".parse().unwrap()
    }

    #[test]
    fn neighbor_refresh_command_flushes_every_address_inside_kamal_proxys_netns() {
        let command = render_neighbor_refresh_command(
            ContainerEngine::Docker,
            &[address(), "100.64.0.11".parse().unwrap()],
        );
        assert!(command.contains("docker inspect -f '{{.State.Pid}}' kamal-proxy"));
        assert!(command.contains("nsenter -t \"$pid\" -n sh -c"));
        assert!(command.contains("ip neigh flush to 100.64.0.10 dev eth0"));
        assert!(command.contains("ip neigh flush to 100.64.0.11 dev eth0"));
    }

    #[test]
    fn neighbor_refresh_command_is_engine_aware() {
        let command = render_neighbor_refresh_command(ContainerEngine::Podman, &[address()]);
        assert!(command.starts_with("pid=$(podman inspect"));
    }

    #[test]
    fn single_target_flat_config_produces_one_route() {
        let address = address();
        let proxy: ProxyConfig =
            serde_yaml::from_str("port: 3000\nhosts: [example.com]\nssl: true\n").unwrap();
        let targets = targets_for_address("demo", "web", Some(&proxy), address);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].route_name, "demo-web-3000");
        assert_eq!(targets[0].hosts, vec!["example.com".to_string()]);
        assert!(targets[0].tls);
    }

    #[test]
    fn multi_target_config_produces_one_route_per_target() {
        let address = address();
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
        let targets = targets_for_address("demo", "web", Some(&proxy), address);
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].route_name, "demo-web-3900");
        assert_eq!(targets[1].route_name, "demo-web-3903");
        assert!(targets[1].tls);
    }

    #[test]
    fn no_proxy_config_means_no_routes() {
        let address = address();
        assert!(targets_for_address("demo", "web", None, address).is_empty());
    }

    #[test]
    fn deploy_command_renders_http_healthcheck() {
        let address = address();
        let proxy: ProxyConfig = serde_yaml::from_str(
            "port: 3000\nhosts: [example.com]\nhealthcheck: { path: /health, interval: 10s, deploy_timeout: 60s }\n",
        )
        .unwrap();
        let target = &targets_for_address("demo", "web", Some(&proxy), address)[0];
        let command = render_deploy_command(ContainerEngine::Docker, target);
        assert!(command.contains("docker exec kamal-proxy kamal-proxy deploy demo-web-3000"));
        assert!(command.starts_with("timeout --signal=TERM --kill-after=5s 65s"));
        assert!(command.contains("--target="));
        assert!(command.contains("--host=example.com"));
        assert!(command.contains("--health-check-path=/health"));
        assert!(command.contains("--health-check-interval=10s"));
        assert!(command.contains("--health-check-timeout=5s"));
        assert!(command.contains("--deploy-timeout=60s"));
        assert!(!command.contains("--health-check-cmd"));
    }

    #[test]
    fn deploy_command_admits_every_unique_replica_address() {
        let address = address();
        let proxy: ProxyConfig = serde_yaml::from_str("port: 3000\n").unwrap();
        let mut target = targets_for_address("demo", "web", Some(&proxy), address).remove(0);
        target.additional_addresses = vec![
            "100.64.0.12".parse().unwrap(),
            "100.64.0.11".parse().unwrap(),
            target.address,
        ];
        let command = render_deploy_command(ContainerEngine::Docker, &target);
        assert_eq!(command.matches("--target=").count(), 3);
        assert!(command.contains("--target=100.64.0.11:3000"));
        assert!(command.contains("--target=100.64.0.12:3000"));
    }

    #[test]
    fn deploy_command_renders_command_healthcheck_with_runtime() {
        let address = address();
        let proxy: ProxyConfig = serde_yaml::from_str(
            "port: 3000\nhosts: [example.com]\nhealthcheck: { cmd: \"test -f /ready\" }\n",
        )
        .unwrap();
        let target = &targets_for_address("demo", "web", Some(&proxy), address)[0];
        let command = render_deploy_command(ContainerEngine::Podman, target);
        assert!(command
            .contains("podman exec --no-session kamal-proxy kamal-proxy deploy demo-web-3000"));
        assert!(command.contains("--health-check-cmd=\"test -f /ready\""));
        assert!(command.contains("--health-check-cmd-runtime=podman"));
    }

    #[test]
    fn podman_route_management_disables_exec_session_tracking() {
        assert_eq!(
            render_remove_command(ContainerEngine::Podman, "demo-web-3000"),
            "podman exec --no-session kamal-proxy kamal-proxy remove demo-web-3000"
        );
        assert_eq!(
            render_list_command(ContainerEngine::Podman),
            "podman exec --no-session kamal-proxy kamal-proxy list"
        );
    }

    #[test]
    fn path_prefix_and_tls_are_rendered_when_set() {
        let address = address();
        let proxy: ProxyConfig = serde_yaml::from_str(
            "port: 3000\nhosts: [example.com]\npath_prefix: /api\nssl: true\n",
        )
        .unwrap();
        let target = &targets_for_address("demo", "web", Some(&proxy), address)[0];
        let command = render_deploy_command(ContainerEngine::Docker, target);
        assert!(command.contains("--path-prefix=/api"));
        assert!(command.contains("--tls"));
    }

    #[test]
    fn agent_runtime_spec_keeps_static_policy_but_not_a_stale_target() {
        let proxy: ProxyConfig = serde_yaml::from_str(
            "port: 3000\nhosts: [example.com]\nssl: true\nhealthcheck: { path: /ready }\n",
        )
        .unwrap();
        let specs = runtime_specs_for_service(ContainerEngine::Podman, "demo", "web", &proxy);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].service, "web");
        assert_eq!(specs[0].route_name, "demo-web-3000");
        assert!(specs[0].deploy_args.contains(&"--host=example.com".into()));
        assert!(specs[0].deploy_args.contains(&"--tls".into()));
        assert!(specs[0]
            .deploy_args
            .contains(&"--health-check-path=/ready".into()));
        assert!(!specs[0]
            .deploy_args
            .iter()
            .any(|argument| argument.starts_with("--target=")));
    }
}
