use anyhow::Context;
use serde::Deserialize;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default = "default_http_listen")]
    pub http_listen: SocketAddr,
    #[serde(default)]
    pub https_listen: Option<SocketAddr>,
    /// Scanned at startup for `{host}.crt`/`{host}.key` pairs an operator
    /// dropped in directly (loaded as permanent, never-renewed entries),
    /// and where ACME writes/renews certificates for routes with `tls:
    /// true` -- see cert_store.rs. Irrelevant if `https_listen` is unset.
    #[serde(default = "default_cert_dir")]
    pub cert_dir: PathBuf,
    #[serde(default)]
    pub acme: Option<AcmeConfig>,
    /// Path to the admin control socket (see admin.rs) that `jiji-proxy
    /// route apply/remove/list` connects to.
    #[serde(default = "crate::admin::default_socket_path")]
    pub admin_socket: PathBuf,
    /// Applied once at startup, before the admin socket accepts any
    /// requests. Routes pushed later through the admin socket (the normal
    /// production path -- see "Control surface" in
    /// `docs/architecture-notes.md#private-networking-wireguard-mesh--agent-served-dns`)
    /// are layered on top of this seed, not replaced by it.
    #[serde(default)]
    pub routes: Vec<RouteConfig>,
    /// Raw TCP routes -- see `crate::tcp_relay`. Same seed-then-layer
    /// semantics as `routes`.
    #[serde(default)]
    pub tcp_routes: Vec<TcpRouteConfig>,
}

/// HTTP-01 only -- see the module doc comment in acme.rs for why DNS-01
/// isn't implemented yet.
#[derive(Debug, Deserialize)]
pub struct AcmeConfig {
    pub directory_url: String,
    #[serde(default)]
    pub contact_email: Option<String>,
    #[serde(default = "default_account_path")]
    pub account_path: PathBuf,
    #[serde(default = "default_renew_before_days")]
    pub renew_before_days: u64,
    #[serde(default = "default_check_interval_secs")]
    pub check_interval_secs: u64,
}

impl AcmeConfig {
    pub fn renew_before(&self) -> Duration {
        Duration::from_secs(self.renew_before_days * 86_400)
    }

    pub fn check_interval(&self) -> Duration {
        Duration::from_secs(self.check_interval_secs)
    }
}

#[derive(Debug, Deserialize)]
pub struct RouteConfig {
    pub host: String,
    /// `None` is the catch-all for `host`; `Some(prefix)` only matches a
    /// request path starting with `prefix` -- see route_manager.rs.
    #[serde(default)]
    pub path_prefix: Option<String>,
    pub discovery: DiscoveryConfig,
    /// Serve/maintain a TLS certificate for this host (operator-supplied
    /// static file in `cert_dir`, or ACME-issued if `acme:` is configured).
    #[serde(default)]
    pub tls: bool,
    /// Active health-checking (see route_manager.rs); omitted means
    /// backends are only ever evicted by DNS re-resolution.
    #[serde(default)]
    pub healthcheck: Option<crate::admin::HealthCheckRequest>,
}

/// Backends are resolved by periodically re-querying `dns_server` for `name`
/// (in production, jiji-agent's local `.jiji` resolver for
/// `{project}-{service}.jiji`), not the host's system resolver -- see
/// discovery.rs. There is deliberately no static-backend fallback: a fixed
/// address per route was phase 1 only, replaced entirely here.
#[derive(Debug, Deserialize)]
pub struct DiscoveryConfig {
    pub dns_server: SocketAddr,
    pub name: String,
    pub port: u16,
    #[serde(default = "default_refresh_interval_secs")]
    pub refresh_interval_secs: u64,
}

/// See `crate::tcp_relay` -- a raw TCP route, keyed by its public
/// `listen_port` rather than a Host header.
#[derive(Debug, Deserialize)]
pub struct TcpRouteConfig {
    pub listen_port: u16,
    pub discovery: DiscoveryConfig,
    #[serde(default)]
    pub healthcheck: Option<crate::admin::HealthCheckRequest>,
}

fn default_refresh_interval_secs() -> u64 {
    5
}

fn default_http_listen() -> SocketAddr {
    "0.0.0.0:8080"
        .parse()
        .expect("valid default listen address")
}

fn default_cert_dir() -> PathBuf {
    PathBuf::from("/etc/jiji/certs")
}

fn default_account_path() -> PathBuf {
    default_cert_dir().join("acme-account.json")
}

fn default_renew_before_days() -> u64 {
    30
}

fn default_check_interval_secs() -> u64 {
    3600
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config file at {}", path.display()))?;
        let config: Config = serde_yaml::from_str(&raw)
            .with_context(|| format!("failed to parse config file at {}", path.display()))?;
        Ok(config)
    }
}
