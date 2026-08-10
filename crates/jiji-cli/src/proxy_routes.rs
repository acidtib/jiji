use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use jiji_config::{ContainerEngine, Environment, HealthcheckConfig, ProxyConfig, SslValue};
use jiji_ssh::SshSession;
use serde::Deserialize;

use crate::container_runtime::exec_prefix;
use crate::env_resolution::ResolvedEnvironment;
use crate::health_check;

/// Placeholder passed to `targets_for_service` when computing
/// `runtime_specs_for_service`'s policy-only args -- the agent always fills
/// in its own `dns_bind_address` at apply time (`local_reconcile.rs`), so
/// the identity/discovery args this placeholder would produce are discarded
/// before they ever reach `ProxyRouteSpec`.
const UNUSED_DNS_SERVER: &str = "0.0.0.0:0";

/// One route jiji-proxy should serve for a service: a single `(host,
/// path_prefix)` pair pointing at `name`'s DNS-discovered backends on
/// `dns_server` -- jiji-proxy resolves and load-balances those backends
/// itself (see
/// `docs/architecture-notes.md#private-networking-wireguard-mesh--agent-served-dns`),
/// so unlike kamal-proxy's route model this carries no explicit target
/// address at all, and applying it doesn't change per deployment.
#[derive(Debug)]
pub struct RouteTarget {
    pub host: String,
    pub path_prefix: Option<String>,
    /// This server's own local jiji-agent `.jiji` resolver address, port 53.
    pub dns_server: SocketAddr,
    /// The aggregate DNS name jiji-proxy re-resolves on its own schedule:
    /// `{project}-{service}.jiji`.
    pub name: String,
    pub port: u16,
    pub ssl: Option<SslValue>,
    pub healthcheck: Option<HealthcheckConfig>,
}

impl RouteTarget {
    fn tls(&self) -> bool {
        matches!(
            self.ssl,
            Some(SslValue::Enabled(true)) | Some(SslValue::Certs { .. })
        )
    }
}

fn is_pem(value: &str) -> bool {
    value.trim_start().starts_with("-----BEGIN ")
}

/// Adds certificate/key environment references to the service's required
/// secrets. Literal PEM values remain supported, but the normal config form
/// names variables from the selected `.env` file.
pub fn add_tls_secret_refs(proxy: Option<&ProxyConfig>, environment: &mut Environment) {
    let mut add = |value: &str| {
        if !is_pem(value) && !environment.secrets.iter().any(|name| name == value) {
            environment.secrets.push(value.to_string());
        }
    };
    let mut inspect = |ssl: Option<&SslValue>| {
        if let Some(SslValue::Certs {
            certificate_pem,
            private_key_pem,
        }) = ssl
        {
            add(certificate_pem);
            add(private_key_pem);
        }
    };
    if let Some(proxy) = proxy {
        inspect(proxy.ssl.as_ref());
        if let Some(targets) = &proxy.targets {
            for target in targets {
                inspect(target.ssl.as_ref());
            }
        }
    }
}

/// Marks TLS environment references as Jiji control-plane inputs so they
/// are never written into the application container's environment file.
pub fn mark_tls_control_secrets(
    proxy: Option<&ProxyConfig>,
    environment: &mut ResolvedEnvironment,
) {
    let mut mark = |ssl: Option<&SslValue>| {
        if let Some(SslValue::Certs {
            certificate_pem,
            private_key_pem,
        }) = ssl
        {
            for value in [certificate_pem, private_key_pem] {
                if !is_pem(value) {
                    environment.control_keys.insert(value.clone());
                }
            }
        }
    };
    if let Some(proxy) = proxy {
        mark(proxy.ssl.as_ref());
        if let Some(targets) = &proxy.targets {
            for target in targets {
                mark(target.ssl.as_ref());
            }
        }
    }
}

/// Replaces certificate/key environment references with their resolved PEM
/// contents before anything is uploaded to a server.
pub fn resolve_tls_secrets(
    targets: &mut [RouteTarget],
    environment: &ResolvedEnvironment,
) -> anyhow::Result<()> {
    for target in targets {
        let Some(SslValue::Certs {
            certificate_pem,
            private_key_pem,
        }) = &mut target.ssl
        else {
            continue;
        };
        for (kind, value) in [
            ("certificate", certificate_pem),
            ("private key", private_key_pem),
        ] {
            if !is_pem(value) {
                *value = environment.values.get(value).cloned().ok_or_else(|| {
                    anyhow::anyhow!(
                        "TLS {kind} variable '{value}' for host '{}' was not resolved",
                        target.host
                    )
                })?;
            }
            if !is_pem(value) {
                anyhow::bail!("TLS {kind} for host '{}' is not PEM data", target.host);
            }
        }
    }
    Ok(())
}

/// Every route a proxy-enabled `service` would register on a host whose
/// local jiji-agent `.jiji` resolver is `dns_server` -- one `RouteTarget`
/// per configured host (kamal-proxy's route model let one route answer for
/// several hosts at once via repeated `--host`; jiji-proxy's admin socket
/// keys a route by exactly one host, so a multi-host `hosts:` list becomes
/// one route apply per host here instead).
pub fn targets_for_service(
    project: &str,
    service_name: &str,
    proxy: Option<&ProxyConfig>,
    dns_server: SocketAddr,
) -> anyhow::Result<Vec<RouteTarget>> {
    let Some(proxy) = proxy else {
        return Ok(Vec::new());
    };
    let name = format!("{project}-{service_name}.jiji");

    if let Some(targets) = &proxy.targets {
        let mut result = Vec::new();
        for target in targets {
            // `listen_port` selects raw TCP mode (see `tcp_targets_for_service`) -- no Host
            // header to route by, so a target that set it never produces an HTTP route here,
            // regardless of whether it also happens to set `hosts` (config validation already
            // rejects `path_prefix`/`ssl` alongside `listen_port`, but `hosts` stays legal there
            // as an informational/DNS-facing value, not a routing key).
            if target.listen_port.is_some() {
                continue;
            }
            let port = u16::try_from(target.port).map_err(|_| {
                anyhow::anyhow!(
                    "service '{service_name}' proxy target port {} is out of range (must fit in 16 bits)",
                    target.port
                )
            })?;
            for host in target.hosts.clone().unwrap_or_default() {
                result.push(RouteTarget {
                    host,
                    path_prefix: target.path_prefix.clone(),
                    dns_server,
                    name: name.clone(),
                    port,
                    ssl: target.ssl.clone(),
                    healthcheck: target.healthcheck.clone(),
                });
            }
        }
        return Ok(result);
    }

    if proxy.listen_port.is_some() {
        return Ok(Vec::new());
    }
    let Some(port) = proxy.port else {
        return Ok(Vec::new());
    };
    let port = u16::try_from(port).map_err(|_| {
        anyhow::anyhow!(
            "service '{service_name}' proxy port {port} is out of range (must fit in 16 bits)"
        )
    })?;
    Ok(proxy
        .hosts
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|host| RouteTarget {
            host,
            path_prefix: proxy.path_prefix.clone(),
            dns_server,
            name: name.clone(),
            port,
            ssl: proxy.ssl.clone(),
            healthcheck: proxy.healthcheck.clone(),
        })
        .collect())
}

/// The subset of `jiji-proxy route apply`'s arguments that don't identify
/// *which* route this is (host/dns-server/name/port/path-prefix) -- just how
/// it should behave (TLS, health-checking). Split out so
/// `runtime_specs_for_service` can hand these to the agent's own
/// `ProxyRouteSpec::apply_args`, which the agent appends to identity args it
/// computes itself (its own `dns_bind_address`, not whatever `dns_server`
/// this target happened to be built with -- see `UNUSED_DNS_SERVER`).
/// `healthcheck.cmd`/`cmd_runtime` are deliberately never translated here:
/// jiji-proxy's own ongoing check is HTTP/TCP-only (see the module doc
/// comment in `jiji-proxy/src/route_manager.rs`) -- `cmd` remains meaningful
/// only for jiji's own pre-activation gate (`health_check.rs`), which execs
/// into the candidate's own container before this route is ever pushed. A
/// `healthcheck:` block with only `cmd` set still enables jiji-proxy's own
/// check, just as a TCP-only probe (`path` absent).
fn policy_args(target: &RouteTarget) -> Vec<String> {
    let mut args = Vec::new();
    if target.tls() {
        args.push("--tls".to_string());
    }
    if let Some(check) = &target.healthcheck {
        args.push("--health-check".to_string());
        if let Some(path) = &check.path {
            args.push(format!("--health-check-path={path}"));
        }
        if let Some(interval) = check
            .interval
            .as_deref()
            .and_then(health_check::parse_duration)
        {
            args.push(format!(
                "--health-check-interval-secs={}",
                interval.as_secs().max(1)
            ));
        }
        if let Some(timeout) = check
            .timeout
            .as_deref()
            .and_then(health_check::parse_duration)
        {
            args.push(format!(
                "--health-check-timeout-secs={}",
                timeout.as_secs().max(1)
            ));
        }
    }
    args
}

/// Renders `jiji-proxy route apply`'s full argument list.
fn render_apply_args(target: &RouteTarget) -> Vec<String> {
    let mut args = vec![
        format!("--host={}", target.host),
        format!("--dns-server={}", target.dns_server),
        format!("--name={}", target.name),
        format!("--port={}", target.port),
    ];
    if let Some(prefix) = &target.path_prefix {
        args.push(format!("--path-prefix={prefix}"));
    }
    args.extend(policy_args(target));
    if matches!(target.ssl, Some(SslValue::Certs { .. })) {
        args.push("--reload-certificate".to_string());
    }
    args
}

/// The `ProxyRouteSpec`s `jiji server setup` bakes into this host's agent
/// mesh config (`commands/server/setup.rs`), so the agent can (re)apply this
/// service's routes itself on every reconcile tick without any CLI-driven
/// SSH call -- see `jiji-agent/src/local_reconcile.rs::reconcile_proxy_routes`.
/// The agent fills in its own `dns_bind_address` at apply time, so
/// `targets_for_service` is called with an unused placeholder here purely to
/// reuse its host/path-prefix/port-validation logic.
pub fn runtime_specs_for_service(
    project: &str,
    service_name: &str,
    proxy: Option<&ProxyConfig>,
) -> anyhow::Result<Vec<jiji_agent::runtime::ProxyRouteSpec>> {
    let placeholder_dns_server: SocketAddr = UNUSED_DNS_SERVER
        .parse()
        .expect("UNUSED_DNS_SERVER is a valid socket address");
    Ok(
        targets_for_service(project, service_name, proxy, placeholder_dns_server)?
            .into_iter()
            .map(|target| jiji_agent::runtime::ProxyRouteSpec {
                host: target.host.clone(),
                path_prefix: target.path_prefix.clone(),
                name: target.name.clone(),
                port: target.port,
                apply_args: {
                    let mut args = policy_args(&target);
                    args.push("--preserve-existing-tls".to_string());
                    args
                },
            })
            .collect(),
    )
}

/// One raw TCP route jiji-proxy should relay for a service: a `listen_port`
/// (the public port) pointing at `name`'s DNS-discovered backends on
/// `dns_server` -- see `crate::tcp_relay` in the jiji-proxy crate. No Host
/// header to route by, so unlike `RouteTarget` there is no `hosts:` fan-out:
/// one config target with `listen_port` set produces exactly one
/// `TcpRouteTarget`.
#[derive(Debug)]
pub struct TcpRouteTarget {
    pub listen_port: u16,
    /// This server's own local jiji-agent `.jiji` resolver address, port 53.
    pub dns_server: SocketAddr,
    /// The aggregate DNS name jiji-proxy re-resolves on its own schedule:
    /// `{project}-{service}.jiji`.
    pub name: String,
    pub port: u16,
    pub healthcheck: Option<HealthcheckConfig>,
}

/// Mirrors `targets_for_service` for raw TCP targets (`listen_port` set).
pub fn tcp_targets_for_service(
    project: &str,
    service_name: &str,
    proxy: Option<&ProxyConfig>,
    dns_server: SocketAddr,
) -> anyhow::Result<Vec<TcpRouteTarget>> {
    let Some(proxy) = proxy else {
        return Ok(Vec::new());
    };
    let name = format!("{project}-{service_name}.jiji");

    if let Some(targets) = &proxy.targets {
        let mut result = Vec::new();
        for target in targets {
            let Some(listen_port) = target.listen_port else {
                continue;
            };
            let port = u16::try_from(target.port).map_err(|_| {
                anyhow::anyhow!(
                    "service '{service_name}' proxy target port {} is out of range (must fit in 16 bits)",
                    target.port
                )
            })?;
            result.push(TcpRouteTarget {
                listen_port,
                dns_server,
                name: name.clone(),
                port,
                healthcheck: target.healthcheck.clone(),
            });
        }
        return Ok(result);
    }

    let Some(listen_port) = proxy.listen_port else {
        return Ok(Vec::new());
    };
    let Some(port) = proxy.port else {
        return Ok(Vec::new());
    };
    let port = u16::try_from(port).map_err(|_| {
        anyhow::anyhow!(
            "service '{service_name}' proxy port {port} is out of range (must fit in 16 bits)"
        )
    })?;
    Ok(vec![TcpRouteTarget {
        listen_port,
        dns_server,
        name,
        port,
        healthcheck: proxy.healthcheck.clone(),
    }])
}

/// Mirrors `policy_args` for TCP routes: health-checking only, no `--tls`
/// (TLS termination for raw TCP is out of scope for v1 -- see
/// `TcpRouteTarget`'s doc comment).
fn tcp_policy_args(target: &TcpRouteTarget) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(check) = &target.healthcheck {
        args.push("--health-check".to_string());
        if let Some(path) = &check.path {
            args.push(format!("--health-check-path={path}"));
        }
        if let Some(interval) = check
            .interval
            .as_deref()
            .and_then(health_check::parse_duration)
        {
            args.push(format!(
                "--health-check-interval-secs={}",
                interval.as_secs().max(1)
            ));
        }
        if let Some(timeout) = check
            .timeout
            .as_deref()
            .and_then(health_check::parse_duration)
        {
            args.push(format!(
                "--health-check-timeout-secs={}",
                timeout.as_secs().max(1)
            ));
        }
    }
    args
}

/// Renders `jiji-proxy tcp-route apply`'s full argument list.
fn render_tcp_apply_args(target: &TcpRouteTarget) -> Vec<String> {
    let mut args = vec![
        format!("--listen-port={}", target.listen_port),
        format!("--dns-server={}", target.dns_server),
        format!("--name={}", target.name),
        format!("--port={}", target.port),
    ];
    args.extend(tcp_policy_args(target));
    args
}

/// Mirrors `runtime_specs_for_service` for raw TCP routes.
pub fn runtime_tcp_specs_for_service(
    project: &str,
    service_name: &str,
    proxy: Option<&ProxyConfig>,
) -> anyhow::Result<Vec<jiji_agent::runtime::TcpRouteSpec>> {
    let placeholder_dns_server: SocketAddr = UNUSED_DNS_SERVER
        .parse()
        .expect("UNUSED_DNS_SERVER is a valid socket address");
    Ok(
        tcp_targets_for_service(project, service_name, proxy, placeholder_dns_server)?
            .into_iter()
            .map(|target| jiji_agent::runtime::TcpRouteSpec {
                listen_port: target.listen_port,
                name: target.name.clone(),
                port: target.port,
                apply_args: tcp_policy_args(&target),
            })
            .collect(),
    )
}

pub fn render_tcp_apply_command(engine: ContainerEngine, target: &TcpRouteTarget) -> String {
    format!(
        "{} jiji-proxy jiji-proxy tcp-route apply {}",
        exec_prefix(engine),
        render_tcp_apply_args(target).join(" ")
    )
}

pub fn render_tcp_remove_command(engine: ContainerEngine, listen_port: u16) -> String {
    format!(
        "{} jiji-proxy jiji-proxy tcp-route remove --listen-port={listen_port}",
        exec_prefix(engine)
    )
}

pub fn render_tcp_list_command(engine: ContainerEngine) -> String {
    format!(
        "{} jiji-proxy jiji-proxy tcp-route list",
        exec_prefix(engine)
    )
}

pub fn render_tcp_status_command(engine: ContainerEngine, listen_port: u16) -> String {
    format!(
        "{} jiji-proxy jiji-proxy tcp-route status --listen-port={listen_port}",
        exec_prefix(engine)
    )
}

pub fn render_apply_command(engine: ContainerEngine, target: &RouteTarget) -> String {
    format!(
        "{} jiji-proxy jiji-proxy route apply {}",
        exec_prefix(engine),
        render_apply_args(target).join(" ")
    )
}

pub fn render_remove_command(
    engine: ContainerEngine,
    host: &str,
    path_prefix: Option<&str>,
) -> String {
    let mut args = vec![format!("--host={host}")];
    if let Some(prefix) = path_prefix {
        args.push(format!("--path-prefix={prefix}"));
    }
    format!(
        "{} jiji-proxy jiji-proxy route remove {}",
        exec_prefix(engine),
        args.join(" ")
    )
}

pub fn render_list_command(engine: ContainerEngine) -> String {
    format!("{} jiji-proxy jiji-proxy route list", exec_prefix(engine))
}

pub fn render_status_command(
    engine: ContainerEngine,
    host: &str,
    path_prefix: Option<&str>,
) -> String {
    let mut args = vec![format!("--host={host}")];
    if let Some(prefix) = path_prefix {
        args.push(format!("--path-prefix={prefix}"));
    }
    format!(
        "{} jiji-proxy jiji-proxy route status {}",
        exec_prefix(engine),
        args.join(" ")
    )
}

pub fn render_version_command(engine: ContainerEngine) -> String {
    format!("{} jiji-proxy jiji-proxy version", exec_prefix(engine))
}

/// Mirrors `jiji_proxy::admin::RouteSummary`'s JSON shape without depending
/// on the `jiji-proxy` crate itself, which would drag its entire runtime
/// dependency tree (Pingora, rustls, the ACME client) into this CLI binary
/// just to parse five fields.
#[derive(Debug, Deserialize)]
pub struct RouteSummary {
    pub host: String,
    #[serde(default)]
    pub path_prefix: Option<String>,
}

/// Mirrors `jiji_proxy::admin::AdminResponse::Status`'s JSON shape (see the
/// `RouteSummary` doc comment above for why this isn't a `jiji-proxy` crate
/// dependency instead).
#[derive(Debug, Deserialize)]
struct RouteStatus {
    route_exists: bool,
    backends: Vec<BackendStatus>,
}

#[derive(Debug, Deserialize)]
struct BackendStatus {
    address: String,
    healthy: bool,
}

/// Mirrors `jiji_proxy::admin::TcpRouteSummary`'s JSON shape (see
/// `RouteSummary`'s doc comment for why this isn't a `jiji-proxy` crate
/// dependency instead).
#[derive(Debug, Deserialize)]
pub struct TcpRouteSummary {
    pub listen_port: u16,
}

/// Mirrors `jiji_proxy::admin::AdminResponse::TcpStatus`'s JSON shape.
#[derive(Debug, Deserialize)]
struct TcpRouteStatus {
    route_exists: bool,
    backends: Vec<BackendStatus>,
}

/// Static PEM certs (`ssl: { certificate_pem, private_key_pem }`) are written
/// into jiji-proxy's `cert_dir` before the route is applied. The apply
/// request reloads them as a `Static` entry (see `cert_store.rs`) before the
/// route requests TLS. ACME never replaces a static entry. This is a no-op
/// for `ssl: true`/absent, where ACME is the certificate source.
async fn upload_static_certs_if_configured(
    session: &SshSession,
    target: &RouteTarget,
) -> anyhow::Result<()> {
    let Some(SslValue::Certs {
        certificate_pem,
        private_key_pem,
    }) = &target.ssl
    else {
        return Ok(());
    };
    upload_cert_file(
        session,
        &format!("{}/{}.crt", jiji_network::CERTS_DIR, target.host),
        certificate_pem.as_bytes(),
    )
    .await?;
    upload_cert_file(
        session,
        &format!("{}/{}.key", jiji_network::CERTS_DIR, target.host),
        private_key_pem.as_bytes(),
    )
    .await
}

async fn upload_cert_file(
    session: &SshSession,
    remote_path: &str,
    content: &[u8],
) -> anyhow::Result<()> {
    let temp = format!("{remote_path}.jiji-new");
    let command = format!("set -eu; install -D -m 0600 /dev/stdin {temp}; mv {temp} {remote_path}");
    let result = session.execute_with_input(&command, content).await?;
    if !result.success {
        anyhow::bail!(
            "Could not write certificate {remote_path} on {}: {}",
            session.host(),
            result.stderr.trim()
        );
    }
    Ok(())
}

pub async fn deploy_route(
    session: &SshSession,
    engine: ContainerEngine,
    target: &RouteTarget,
) -> anyhow::Result<()> {
    upload_static_certs_if_configured(session, target).await?;
    let command = render_apply_command(engine, target);
    let result = session.execute(&command).await?;
    if !result.success {
        anyhow::bail!(
            "Could not apply proxy route for host '{}' on {}: {}",
            target.host,
            session.host(),
            result.stderr.trim()
        );
    }
    Ok(())
}

pub async fn remove_route(
    session: &SshSession,
    engine: ContainerEngine,
    host: &str,
    path_prefix: Option<&str>,
) -> anyhow::Result<()> {
    let command = render_remove_command(engine, host, path_prefix);
    let result = session.execute(&command).await?;
    if !result.success {
        anyhow::bail!(
            "Could not remove proxy route for host '{host}' on {}: {}",
            session.host(),
            result.stderr.trim()
        );
    }
    Ok(())
}

pub async fn list_routes(
    session: &SshSession,
    engine: ContainerEngine,
) -> anyhow::Result<Vec<RouteSummary>> {
    let command = render_list_command(engine);
    let result = session.execute(&command).await?;
    if !result.success {
        anyhow::bail!(
            "Could not list jiji-proxy routes on {}: {}",
            session.host(),
            result.stderr.trim()
        );
    }
    serde_json::from_str(&result.stdout).map_err(|error| {
        anyhow::anyhow!(
            "Could not parse jiji-proxy's route list output from {}: {error}",
            session.host()
        )
    })
}

pub async fn verify_route(
    session: &SshSession,
    engine: ContainerEngine,
    host: &str,
    path_prefix: Option<&str>,
) -> anyhow::Result<()> {
    let routes = list_routes(session, engine).await?;
    let found = routes
        .iter()
        .any(|route| route.host == host && route.path_prefix.as_deref() == path_prefix);
    if !found {
        anyhow::bail!(
            "Proxy route for host '{host}' is not listed by jiji-proxy on {}. Inspect it with `{} jiji-proxy jiji-proxy route list`.",
            session.host(),
            exec_prefix(engine)
        );
    }
    Ok(())
}

pub async fn deploy_tcp_route(
    session: &SshSession,
    engine: ContainerEngine,
    target: &TcpRouteTarget,
) -> anyhow::Result<()> {
    let command = render_tcp_apply_command(engine, target);
    let result = session.execute(&command).await?;
    if !result.success {
        anyhow::bail!(
            "Could not apply tcp proxy route for listen_port {} on {}: {}",
            target.listen_port,
            session.host(),
            result.stderr.trim()
        );
    }
    Ok(())
}

pub async fn remove_tcp_route(
    session: &SshSession,
    engine: ContainerEngine,
    listen_port: u16,
) -> anyhow::Result<()> {
    let command = render_tcp_remove_command(engine, listen_port);
    let result = session.execute(&command).await?;
    if !result.success {
        anyhow::bail!(
            "Could not remove tcp proxy route for listen_port {listen_port} on {}: {}",
            session.host(),
            result.stderr.trim()
        );
    }
    Ok(())
}

pub async fn list_tcp_routes(
    session: &SshSession,
    engine: ContainerEngine,
) -> anyhow::Result<Vec<TcpRouteSummary>> {
    let command = render_tcp_list_command(engine);
    let result = session.execute(&command).await?;
    if !result.success {
        anyhow::bail!(
            "Could not list jiji-proxy tcp routes on {}: {}",
            session.host(),
            result.stderr.trim()
        );
    }
    serde_json::from_str(&result.stdout).map_err(|error| {
        anyhow::anyhow!(
            "Could not parse jiji-proxy's tcp route list output from {}: {error}",
            session.host()
        )
    })
}

pub async fn verify_tcp_route(
    session: &SshSession,
    engine: ContainerEngine,
    listen_port: u16,
) -> anyhow::Result<()> {
    let routes = list_tcp_routes(session, engine).await?;
    let found = routes.iter().any(|route| route.listen_port == listen_port);
    if !found {
        anyhow::bail!(
            "TCP proxy route for listen_port {listen_port} is not listed by jiji-proxy on {}. Inspect it with `{} jiji-proxy jiji-proxy tcp-route list`.",
            session.host(),
            exec_prefix(engine)
        );
    }
    Ok(())
}

/// Rejects a `jiji-proxy` running below `crate::version_requirements::
/// MIN_PROXY_VERSION`, actionable ("Run `jiji proxy restart` to update
/// it."): a stale proxy left behind after the local `jiji` CLI itself was
/// upgraded is otherwise a silent compatibility risk with no signal to the
/// operator.
pub async fn check_version(
    session: &SshSession,
    engine: ContainerEngine,
    host: &str,
) -> anyhow::Result<()> {
    let command = render_version_command(engine);
    let result = session.execute(&command).await?;
    if !result.success {
        anyhow::bail!(
            "Could not read jiji-proxy's version on {}: {}",
            session.host(),
            result.stderr.trim()
        );
    }
    let version = result.stdout.trim();
    crate::version_requirements::check_min_version(
        "jiji-proxy",
        host,
        version,
        crate::version_requirements::MIN_PROXY_VERSION,
        "Run `jiji proxy restart` to update it.",
    )
}

async fn route_status(
    session: &SshSession,
    engine: ContainerEngine,
    host: &str,
    path_prefix: Option<&str>,
) -> anyhow::Result<RouteStatus> {
    let command = render_status_command(engine, host, path_prefix);
    let result = session.execute(&command).await?;
    if !result.success {
        anyhow::bail!(
            "Could not read jiji-proxy route status for host '{host}' on {}: {}",
            session.host(),
            result.stderr.trim()
        );
    }
    serde_json::from_str(&result.stdout).map_err(|error| {
        anyhow::anyhow!(
            "Could not parse jiji-proxy's route status output from {}: {error}",
            session.host()
        )
    })
}

/// Zero-downtime activation barrier: polls `jiji-proxy route status` until
/// `expected_address` shows up as a healthy backend for `(host,
/// path_prefix)`, or `timeout` elapses. Under jiji-proxy's DNS-driven model
/// a route's static definition never carries an address, so this is what
/// replaces kamal-proxy's own blocking `deploy` -- called by
/// `deploy_transaction.rs` right after the candidate is committed
/// Active/Healthy in the catalog (so the DNS answer this polls against
/// already reflects it), immediately after a `deploy_route` re-apply that
/// forces jiji-proxy to re-resolve now rather than waiting out
/// `refresh_interval_secs`.
pub async fn verify_route_address(
    session: &SshSession,
    engine: ContainerEngine,
    host: &str,
    path_prefix: Option<&str>,
    expected_address: SocketAddr,
    timeout: Duration,
) -> anyhow::Result<()> {
    let expected = expected_address.to_string();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let status = route_status(session, engine, host, path_prefix).await?;
        if !status.route_exists {
            anyhow::bail!(
                "Proxy route for host '{host}' is not registered on {}",
                session.host()
            );
        }
        if status
            .backends
            .iter()
            .any(|backend| backend.address == expected && backend.healthy)
        {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            let seen: Vec<&str> = status
                .backends
                .iter()
                .map(|backend| backend.address.as_str())
                .collect();
            anyhow::bail!(
                "jiji-proxy on {} did not report {expected} as a healthy backend for host '{host}' within {}s (currently sees: [{}])",
                session.host(),
                timeout.as_secs(),
                seen.join(", ")
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn tcp_route_status(
    session: &SshSession,
    engine: ContainerEngine,
    listen_port: u16,
) -> anyhow::Result<TcpRouteStatus> {
    let command = render_tcp_status_command(engine, listen_port);
    let result = session.execute(&command).await?;
    if !result.success {
        anyhow::bail!(
            "Could not read jiji-proxy tcp route status for listen_port {listen_port} on {}: {}",
            session.host(),
            result.stderr.trim()
        );
    }
    serde_json::from_str(&result.stdout).map_err(|error| {
        anyhow::anyhow!(
            "Could not parse jiji-proxy's tcp route status output from {}: {error}",
            session.host()
        )
    })
}

/// Mirrors `verify_route_address` for a raw TCP route, keyed by
/// `listen_port` instead of `(host, path_prefix)`.
pub async fn verify_tcp_route_address(
    session: &SshSession,
    engine: ContainerEngine,
    listen_port: u16,
    expected_address: SocketAddr,
    timeout: Duration,
) -> anyhow::Result<()> {
    let expected = expected_address.to_string();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let status = tcp_route_status(session, engine, listen_port).await?;
        if !status.route_exists {
            anyhow::bail!(
                "TCP proxy route for listen_port {listen_port} is not registered on {}",
                session.host()
            );
        }
        if status
            .backends
            .iter()
            .any(|backend| backend.address == expected && backend.healthy)
        {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            let seen: Vec<&str> = status
                .backends
                .iter()
                .map(|backend| backend.address.as_str())
                .collect();
            anyhow::bail!(
                "jiji-proxy on {} did not report {expected} as a healthy backend for listen_port {listen_port} within {}s (currently sees: [{}])",
                session.host(),
                timeout.as_secs(),
                seen.join(", ")
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Rebuild every selected service's routes on each host from that host's
/// own local jiji-agent DNS resolver. Unlike kamal-proxy's per-deploy
/// address push, this only needs to run when a route's static definition
/// (host/path/port/tls/healthcheck) might have changed -- backend churn is
/// handled entirely by jiji-proxy's own continuous DNS re-resolution, not
/// by this function reading the catalog at all. jiji-proxy resolves the
/// aggregate `{project}-{service}.jiji` name mesh-wide (jiji-agent's
/// catalog is replicated to every host), so -- unlike kamal-proxy, which
/// was restricted to a host's own local replicas -- every selected host
/// gets a real route regardless of whether it happens to run a replica
/// itself; this is what restores genuine cross-host load balancing.
pub async fn reconcile_catalog_routes(
    sessions: &BTreeMap<String, Arc<SshSession>>,
    dns_servers: &BTreeMap<String, SocketAddr>,
    project: &str,
    engine: ContainerEngine,
    services: &BTreeMap<String, ProxyConfig>,
    resolved_envs: &BTreeMap<String, ResolvedEnvironment>,
) -> anyhow::Result<()> {
    if services.is_empty() || sessions.is_empty() {
        return Ok(());
    }
    for (host, session) in sessions {
        let Some(&dns_server) = dns_servers.get(host) else {
            anyhow::bail!("no DNS server address known for server '{host}'");
        };
        for (service, proxy) in services {
            let mut targets = targets_for_service(project, service, Some(proxy), dns_server)?;
            let resolved = resolved_envs.get(service).ok_or_else(|| {
                anyhow::anyhow!("no resolved environment available for service '{service}'")
            })?;
            resolve_tls_secrets(&mut targets, resolved)?;
            for target in targets {
                deploy_route(session, engine, &target).await?;
                verify_route(session, engine, &target.host, target.path_prefix.as_deref()).await?;
            }
            for target in tcp_targets_for_service(project, service, Some(proxy), dns_server)? {
                deploy_tcp_route(session, engine, &target).await?;
                verify_tcp_route(session, engine, target.listen_port).await?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dns_server() -> SocketAddr {
        "100.64.0.2:53".parse().unwrap()
    }

    #[test]
    fn single_target_flat_config_produces_one_route_per_host() {
        let proxy: ProxyConfig =
            serde_yaml::from_str("port: 3000\nhosts: [example.com]\nssl: true\n").unwrap();
        let targets = targets_for_service("demo", "web", Some(&proxy), dns_server()).unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].host, "example.com");
        assert_eq!(targets[0].name, "demo-web.jiji");
        assert_eq!(targets[0].port, 3000);
        assert!(targets[0].tls());
    }

    #[test]
    fn static_tls_refs_are_required_and_resolved_to_pem() {
        let proxy: ProxyConfig = serde_yaml::from_str(
            "port: 3000\nhosts: [example.com]\nssl: { certificate_pem: CERT, private_key_pem: KEY }\n",
        )
        .unwrap();
        let mut environment = Environment::default();
        add_tls_secret_refs(Some(&proxy), &mut environment);
        assert_eq!(environment.secrets, vec!["CERT", "KEY"]);

        let mut resolved = ResolvedEnvironment::default();
        resolved.values.insert(
            "CERT".to_string(),
            "-----BEGIN CERTIFICATE-----\ndata\n-----END CERTIFICATE-----".to_string(),
        );
        resolved.values.insert(
            "KEY".to_string(),
            "-----BEGIN PRIVATE KEY-----\ndata\n-----END PRIVATE KEY-----".to_string(),
        );
        let mut targets = targets_for_service("demo", "web", Some(&proxy), dns_server()).unwrap();
        resolve_tls_secrets(&mut targets, &resolved).unwrap();

        let Some(SslValue::Certs {
            certificate_pem,
            private_key_pem,
        }) = &targets[0].ssl
        else {
            panic!("expected static TLS");
        };
        assert!(certificate_pem.starts_with("-----BEGIN CERTIFICATE-----"));
        assert!(private_key_pem.starts_with("-----BEGIN PRIVATE KEY-----"));
        assert!(render_apply_args(&targets[0])
            .iter()
            .any(|arg| arg == "--reload-certificate"));
    }

    #[test]
    fn multiple_hosts_produce_one_route_each() {
        let proxy: ProxyConfig =
            serde_yaml::from_str("port: 3000\nhosts: [a.example.com, b.example.com]\n").unwrap();
        let targets = targets_for_service("demo", "web", Some(&proxy), dns_server()).unwrap();
        let hosts: Vec<&str> = targets.iter().map(|target| target.host.as_str()).collect();
        assert_eq!(hosts, vec!["a.example.com", "b.example.com"]);
    }

    #[test]
    fn multi_target_config_produces_one_route_per_target_host() {
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
        let targets = targets_for_service("demo", "web", Some(&proxy), dns_server()).unwrap();
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].host, "s3.example.com");
        assert_eq!(targets[0].port, 3900);
        assert_eq!(targets[1].host, "admin.example.com");
        assert!(targets[1].tls());
    }

    #[test]
    fn no_proxy_config_means_no_routes() {
        assert!(targets_for_service("demo", "web", None, dns_server())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn apply_command_renders_http_healthcheck() {
        let proxy: ProxyConfig = serde_yaml::from_str(
            "port: 3000\nhosts: [example.com]\nhealthcheck: { path: /health, interval: 10s, timeout: 3s }\n",
        )
        .unwrap();
        let target = &targets_for_service("demo", "web", Some(&proxy), dns_server()).unwrap()[0];
        let command = render_apply_command(ContainerEngine::Docker, target);
        assert!(command.starts_with("docker exec jiji-proxy jiji-proxy route apply"));
        assert!(command.contains("--host=example.com"));
        assert!(command.contains("--dns-server=100.64.0.2:53"));
        assert!(command.contains("--name=demo-web.jiji"));
        assert!(command.contains("--port=3000"));
        assert!(command.contains("--health-check "));
        assert!(command.contains("--health-check-path=/health"));
        assert!(command.contains("--health-check-interval-secs=10"));
        assert!(command.contains("--health-check-timeout-secs=3"));
    }

    #[test]
    fn apply_command_renders_path_prefix_and_tls() {
        let proxy: ProxyConfig = serde_yaml::from_str(
            "port: 3000\nhosts: [example.com]\npath_prefix: /api\nssl: true\n",
        )
        .unwrap();
        let target = &targets_for_service("demo", "web", Some(&proxy), dns_server()).unwrap()[0];
        let command = render_apply_command(ContainerEngine::Docker, target);
        assert!(command.contains("--path-prefix=/api"));
        assert!(command.contains("--tls"));
    }

    #[test]
    fn cmd_only_healthcheck_still_enables_a_tcp_only_check() {
        let proxy: ProxyConfig = serde_yaml::from_str(
            "port: 3000\nhosts: [example.com]\nhealthcheck: { cmd: \"test -f /ready\" }\n",
        )
        .unwrap();
        let target = &targets_for_service("demo", "web", Some(&proxy), dns_server()).unwrap()[0];
        let command = render_apply_command(ContainerEngine::Podman, target);
        assert!(command.contains("podman exec --no-session jiji-proxy jiji-proxy route apply"));
        assert!(command.contains("--health-check"));
        assert!(!command.contains("--health-check-path"));
    }

    #[test]
    fn podman_route_management_disables_exec_session_tracking() {
        assert_eq!(
            render_remove_command(ContainerEngine::Podman, "example.com", None),
            "podman exec --no-session jiji-proxy jiji-proxy route remove --host=example.com"
        );
        assert_eq!(
            render_list_command(ContainerEngine::Podman),
            "podman exec --no-session jiji-proxy jiji-proxy route list"
        );
    }

    #[test]
    fn out_of_range_port_is_rejected_clearly() {
        let proxy: ProxyConfig =
            serde_yaml::from_str("port: 99999\nhosts: [example.com]\n").unwrap();
        let error = targets_for_service("demo", "web", Some(&proxy), dns_server()).unwrap_err();
        assert!(error.to_string().contains("out of range"));
    }

    #[test]
    fn tcp_target_flat_config_produces_one_tcp_route() {
        let proxy: ProxyConfig = serde_yaml::from_str("port: 5432\nlisten_port: 5432\n").unwrap();
        let targets = tcp_targets_for_service("demo", "db", Some(&proxy), dns_server()).unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].listen_port, 5432);
        assert_eq!(targets[0].name, "demo-db.jiji");
        assert_eq!(targets[0].port, 5432);
    }

    #[test]
    fn tcp_target_is_never_also_returned_as_an_http_route() {
        let proxy: ProxyConfig =
            serde_yaml::from_str("port: 5432\nlisten_port: 5432\nhosts: [db.example.com]\n")
                .unwrap();
        assert!(
            targets_for_service("demo", "db", Some(&proxy), dns_server())
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            tcp_targets_for_service("demo", "db", Some(&proxy), dns_server())
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn multi_target_config_separates_http_and_tcp_targets() {
        let proxy: ProxyConfig = serde_yaml::from_str(
            r#"
targets:
  - port: 80
    hosts: [web.example.com]
  - port: 5432
    listen_port: 5432
"#,
        )
        .unwrap();
        let http_targets = targets_for_service("demo", "app", Some(&proxy), dns_server()).unwrap();
        assert_eq!(http_targets.len(), 1);
        assert_eq!(http_targets[0].host, "web.example.com");

        let tcp_targets =
            tcp_targets_for_service("demo", "app", Some(&proxy), dns_server()).unwrap();
        assert_eq!(tcp_targets.len(), 1);
        assert_eq!(tcp_targets[0].listen_port, 5432);
    }

    #[test]
    fn no_proxy_config_means_no_tcp_routes() {
        assert!(tcp_targets_for_service("demo", "db", None, dns_server())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn tcp_apply_command_renders_listen_port_and_healthcheck() {
        let proxy: ProxyConfig = serde_yaml::from_str(
            "port: 5432\nlisten_port: 5432\nhealthcheck: { cmd: \"pg_isready\", interval: 10s }\n",
        )
        .unwrap();
        let target = &tcp_targets_for_service("demo", "db", Some(&proxy), dns_server()).unwrap()[0];
        let command = render_tcp_apply_command(ContainerEngine::Docker, target);
        assert!(command.starts_with("docker exec jiji-proxy jiji-proxy tcp-route apply"));
        assert!(command.contains("--listen-port=5432"));
        assert!(command.contains("--dns-server=100.64.0.2:53"));
        assert!(command.contains("--name=demo-db.jiji"));
        assert!(command.contains("--port=5432"));
        assert!(command.contains("--health-check"));
        assert!(command.contains("--health-check-interval-secs=10"));
        // No --host/--path-prefix/--tls: TCP routes have no Host header to route by.
        assert!(!command.contains("--host="));
        assert!(!command.contains("--path-prefix"));
        assert!(!command.contains("--tls"));
    }

    #[test]
    fn tcp_route_management_commands_render_correctly() {
        assert_eq!(
            render_tcp_remove_command(ContainerEngine::Podman, 5432),
            "podman exec --no-session jiji-proxy jiji-proxy tcp-route remove --listen-port=5432"
        );
        assert_eq!(
            render_tcp_list_command(ContainerEngine::Podman),
            "podman exec --no-session jiji-proxy jiji-proxy tcp-route list"
        );
        assert_eq!(
            render_tcp_status_command(ContainerEngine::Docker, 5432),
            "docker exec jiji-proxy jiji-proxy tcp-route status --listen-port=5432"
        );
    }

    #[test]
    fn tcp_out_of_range_port_is_rejected_clearly() {
        let proxy: ProxyConfig = serde_yaml::from_str("port: 99999\nlisten_port: 5432\n").unwrap();
        let error = tcp_targets_for_service("demo", "db", Some(&proxy), dns_server()).unwrap_err();
        assert!(error.to_string().contains("out of range"));
    }
}
