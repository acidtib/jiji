//! jiji-proxy's control surface: a length-prefixed JSON request/response
//! protocol over a container-local Unix socket, the same framing shape as
//! jiji-agent's own API (`jiji-agent/src/api.rs`). Reached via `docker exec
//! jiji-proxy jiji-proxy route ...` from jiji-cli/jiji-agent, mirroring how
//! `docker exec kamal-proxy kamal-proxy deploy ...` reaches kamal-proxy's
//! own admin API today -- this replaces that call, not the exec pattern
//! itself. See "Control surface" in plans/jiji-proxy-design.md.

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;

use crate::route_manager::HealthCheckSpec;

/// A declared frame length above this is rejected before the body is read.
pub const MAX_REQUEST_BYTES: u32 = 64 * 1024;

pub const DEFAULT_SOCKET_PATH: &str = "/run/jiji-proxy/admin.sock";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AdminRequest {
    RouteApply {
        host: String,
        /// `None` is the catch-all for `host`; `Some(prefix)` only matches
        /// a request path starting with `prefix` -- see route_manager.rs.
        #[serde(default)]
        path_prefix: Option<String>,
        dns_server: SocketAddr,
        name: String,
        port: u16,
        #[serde(default = "default_refresh_interval_secs")]
        refresh_interval_secs: u64,
        /// Whether to serve/maintain a TLS certificate for this host (see
        /// acme.rs). Defaults to `false` -- most routes are backend
        /// targets, not TLS-terminated hosts in their own right.
        #[serde(default)]
        tls: bool,
        /// Active health-checking (see route_manager.rs). Omitted means
        /// backends are only ever evicted by DNS re-resolution, not by
        /// jiji-proxy's own probing.
        #[serde(default)]
        health_check: Option<HealthCheckRequest>,
    },
    RouteRemove {
        host: String,
        #[serde(default)]
        path_prefix: Option<String>,
    },
    RouteList,
    /// Current backend addresses for one exact route, each paired with
    /// whether `select()` currently considers it ready. Used by jiji-cli
    /// after a deploy's catalog commit to confirm a specific address is
    /// actually discovered and healthy, rather than trusting a re-`apply`'s
    /// bare success -- `apply` always runs an initial synchronous
    /// discovery/health-check pass (see route_manager.rs), so re-issuing it
    /// after the catalog write forces jiji-proxy to re-resolve DNS
    /// immediately instead of waiting out `refresh_interval_secs`; this
    /// request is what confirms that re-resolution actually surfaced the
    /// new address.
    RouteStatus {
        host: String,
        #[serde(default)]
        path_prefix: Option<String>,
    },
    /// This binary's own `CARGO_PKG_VERSION` -- used by jiji-cli to reject
    /// an already-running jiji-proxy below its required minimum version
    /// (see `crate::version_requirements` in the jiji-cli crate).
    Version,
    /// Adds or replaces the raw TCP route for `listen_port` -- see
    /// `crate::tcp_relay`. Rejected if `listen_port` is already claimed by
    /// a *different* `name` (see `RouteManager::tcp_apply`).
    TcpRouteApply {
        listen_port: u16,
        dns_server: SocketAddr,
        name: String,
        port: u16,
        #[serde(default = "default_refresh_interval_secs")]
        refresh_interval_secs: u64,
        #[serde(default)]
        health_check: Option<HealthCheckRequest>,
    },
    TcpRouteRemove {
        listen_port: u16,
    },
    TcpRouteList,
    /// Mirrors `RouteStatus` for the TCP table, keyed by `listen_port`.
    TcpRouteStatus {
        listen_port: u16,
    },
}

fn default_refresh_interval_secs() -> u64 {
    5
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheckRequest {
    /// HTTP GET path to check; `None` checks TCP connectivity only.
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default = "default_health_check_interval_secs")]
    pub interval_secs: u64,
    #[serde(default = "default_health_check_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default = "default_consecutive_count")]
    pub consecutive_success: usize,
    #[serde(default = "default_consecutive_count")]
    pub consecutive_failure: usize,
}

fn default_health_check_interval_secs() -> u64 {
    5
}

fn default_health_check_timeout_secs() -> u64 {
    2
}

fn default_consecutive_count() -> usize {
    1
}

impl From<HealthCheckRequest> for HealthCheckSpec {
    fn from(request: HealthCheckRequest) -> Self {
        HealthCheckSpec {
            path: request.path,
            interval: Duration::from_secs(request.interval_secs.max(1)),
            timeout: Duration::from_secs(request.timeout_secs.max(1)),
            consecutive_success: request.consecutive_success.max(1),
            consecutive_failure: request.consecutive_failure.max(1),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AdminResponse {
    Ok,
    Routes {
        routes: Vec<RouteSummary>,
    },
    /// Response to `RouteStatus`. `None` (`route_exists: false`) means no
    /// such `(host, path_prefix)` route is currently registered.
    Status {
        route_exists: bool,
        backends: Vec<BackendStatus>,
    },
    Version {
        version: String,
    },
    Error {
        message: String,
    },
    TcpRoutes {
        routes: Vec<TcpRouteSummary>,
    },
    /// Response to `TcpRouteStatus`, mirrors `Status`.
    TcpStatus {
        route_exists: bool,
        backends: Vec<BackendStatus>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendStatus {
    pub address: String,
    pub healthy: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteSummary {
    pub host: String,
    pub path_prefix: Option<String>,
    pub dns_server: SocketAddr,
    pub name: String,
    pub port: u16,
    pub tls: bool,
    pub health_check: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TcpRouteSummary {
    pub listen_port: u16,
    pub dns_server: SocketAddr,
    pub name: String,
    pub port: u16,
    pub health_check: bool,
}

pub async fn serve(
    socket_path: &Path,
    manager: crate::route_manager::RouteManager,
    mut shutdown: watch::Receiver<bool>,
) {
    let listener = match bind_socket(socket_path).await {
        Ok(listener) => listener,
        Err(error) => {
            tracing::error!(%error, path = %socket_path.display(), "failed to bind jiji-proxy admin socket");
            return;
        }
    };
    tracing::info!(path = %socket_path.display(), "admin socket listening");

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _)) => {
                        let manager = manager.clone();
                        tokio::spawn(async move { handle_connection(stream, manager).await; });
                    }
                    Err(error) => tracing::warn!(%error, "admin socket accept failed"),
                }
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    return;
                }
            }
        }
    }
}

async fn handle_connection(mut stream: UnixStream, manager: crate::route_manager::RouteManager) {
    loop {
        let request = match read_request(&mut stream).await {
            Ok(Some(request)) => request,
            Ok(None) => return,
            Err(message) => {
                let _ = write_response(&mut stream, &AdminResponse::Error { message }).await;
                return;
            }
        };
        let response = handle_request(&manager, request).await;
        if write_response(&mut stream, &response).await.is_err() {
            return;
        }
    }
}

async fn handle_request(
    manager: &crate::route_manager::RouteManager,
    request: AdminRequest,
) -> AdminResponse {
    match request {
        AdminRequest::RouteApply {
            host,
            path_prefix,
            dns_server,
            name,
            port,
            refresh_interval_secs,
            tls,
            health_check,
        } => match manager
            .apply(
                host,
                path_prefix,
                dns_server,
                name,
                port,
                refresh_interval_secs,
                tls,
                health_check.map(HealthCheckSpec::from),
            )
            .await
        {
            Ok(()) => AdminResponse::Ok,
            Err(error) => AdminResponse::Error {
                message: error.to_string(),
            },
        },
        AdminRequest::RouteRemove { host, path_prefix } => {
            manager.remove(&host, path_prefix.as_deref());
            AdminResponse::Ok
        }
        AdminRequest::RouteList => {
            let routes = manager
                .list()
                .into_iter()
                .map(
                    |(host, path_prefix, dns_server, name, port, tls, health_check)| RouteSummary {
                        host,
                        path_prefix,
                        dns_server,
                        name,
                        port,
                        tls,
                        health_check,
                    },
                )
                .collect();
            AdminResponse::Routes { routes }
        }
        AdminRequest::RouteStatus { host, path_prefix } => {
            match manager.backend_status(&host, path_prefix.as_deref()) {
                Some(backends) => AdminResponse::Status {
                    route_exists: true,
                    backends: backends
                        .into_iter()
                        .map(|(address, healthy)| BackendStatus { address, healthy })
                        .collect(),
                },
                None => AdminResponse::Status {
                    route_exists: false,
                    backends: Vec::new(),
                },
            }
        }
        AdminRequest::Version => AdminResponse::Version {
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        AdminRequest::TcpRouteApply {
            listen_port,
            dns_server,
            name,
            port,
            refresh_interval_secs,
            health_check,
        } => match manager
            .tcp_apply(
                listen_port,
                dns_server,
                name,
                port,
                refresh_interval_secs,
                health_check.map(HealthCheckSpec::from),
            )
            .await
        {
            Ok(()) => AdminResponse::Ok,
            Err(error) => AdminResponse::Error {
                message: error.to_string(),
            },
        },
        AdminRequest::TcpRouteRemove { listen_port } => {
            manager.tcp_remove(listen_port);
            AdminResponse::Ok
        }
        AdminRequest::TcpRouteList => {
            let routes = manager
                .tcp_list()
                .into_iter()
                .map(
                    |(listen_port, dns_server, name, port, health_check)| TcpRouteSummary {
                        listen_port,
                        dns_server,
                        name,
                        port,
                        health_check,
                    },
                )
                .collect();
            AdminResponse::TcpRoutes { routes }
        }
        AdminRequest::TcpRouteStatus { listen_port } => {
            match manager.tcp_backend_status(listen_port) {
                Some(backends) => AdminResponse::TcpStatus {
                    route_exists: true,
                    backends: backends
                        .into_iter()
                        .map(|(address, healthy)| BackendStatus { address, healthy })
                        .collect(),
                },
                None => AdminResponse::TcpStatus {
                    route_exists: false,
                    backends: Vec::new(),
                },
            }
        }
    }
}

/// A single request/response exchange; opens a fresh connection per call,
/// matching jiji-agent's own `api::call` (used by the `jiji-proxy route`
/// subcommands, each a short-lived process invoked over `docker exec`).
pub async fn call(socket_path: &Path, request: &AdminRequest) -> anyhow::Result<AdminResponse> {
    let mut stream = UnixStream::connect(socket_path).await.map_err(|error| {
        anyhow::anyhow!(
            "could not reach jiji-proxy admin socket at {}: {error}",
            socket_path.display()
        )
    })?;
    let payload = serde_json::to_vec(request)?;
    stream
        .write_all(&(payload.len() as u32).to_be_bytes())
        .await?;
    stream.write_all(&payload).await?;
    stream.flush().await?;

    read_response(&mut stream)
        .await
        .map_err(|error| {
            anyhow::anyhow!("jiji-proxy admin socket returned an unreadable response: {error}")
        })?
        .ok_or_else(|| {
            anyhow::anyhow!("jiji-proxy admin socket closed the connection without a response")
        })
}

async fn read_request(stream: &mut UnixStream) -> Result<Option<AdminRequest>, String> {
    let Some(body) = read_frame(stream)
        .await
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|error| format!("invalid request: {error}"))
}

async fn read_response(stream: &mut UnixStream) -> std::io::Result<Option<AdminResponse>> {
    let Some(body) = read_frame(stream).await? else {
        return Ok(None);
    };
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

async fn read_frame(stream: &mut UnixStream) -> std::io::Result<Option<Vec<u8>>> {
    let mut length_bytes = [0u8; 4];
    match stream.read_exact(&mut length_bytes).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let length = u32::from_be_bytes(length_bytes);
    if length > MAX_REQUEST_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("frame of {length} bytes exceeds the {MAX_REQUEST_BYTES}-byte limit"),
        ));
    }
    let mut body = vec![0u8; length as usize];
    stream.read_exact(&mut body).await?;
    Ok(Some(body))
}

async fn write_response(stream: &mut UnixStream, response: &AdminResponse) -> std::io::Result<()> {
    let payload = serde_json::to_vec(response).unwrap_or_else(|_| {
        br#"{"type":"error","message":"response serialization failed"}"#.to_vec()
    });
    stream
        .write_all(&(payload.len() as u32).to_be_bytes())
        .await?;
    stream.write_all(&payload).await?;
    stream.flush().await
}

/// Replaces a stale leftover socket file from an unclean previous shutdown
/// but never one still owned by a live process -- mirrors jiji-agent's own
/// `bind_socket` (`jiji-agent/src/main.rs`).
async fn bind_socket(socket_path: &Path) -> anyhow::Result<UnixListener> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if socket_path.exists() {
        if UnixStream::connect(socket_path).await.is_ok() {
            anyhow::bail!(
                "another jiji-proxy is already listening on {}; refusing to start a second instance",
                socket_path.display()
            );
        }
        std::fs::remove_file(socket_path)?;
    }
    Ok(UnixListener::bind(socket_path)?)
}

pub fn default_socket_path() -> PathBuf {
    PathBuf::from(DEFAULT_SOCKET_PATH)
}
