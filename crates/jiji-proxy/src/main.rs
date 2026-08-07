use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use jiji_proxy::admin::{self, AdminRequest, AdminResponse, HealthCheckRequest};
use jiji_proxy::cert_store::DynamicCertResolver;
use jiji_proxy::route_manager::HealthCheckSpec;
use jiji_proxy::{
    AcmeManager, CertStore, Config, JijiProxy, JijiTcpProxy, PendingChallenges, RouteManager,
    TCP_RELAY_PORT,
};
use pingora::listeners::tls::TlsSettings;
use pingora::listeners::TlsAcceptCallbacks;
use pingora::prelude::*;
use pingora::services::background::background_service;

#[derive(Parser)]
#[command(name = "jiji-proxy", about = "Jiji's Pingora-based ingress proxy")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Runs the proxy daemon in the foreground (the container's entrypoint).
    Run {
        /// Path to the listener config (see config.example.yml).
        #[arg(long, default_value = "/etc/jiji/proxy/config.yml")]
        config: PathBuf,
    },
    /// Talks to a running jiji-proxy over its admin socket. Invoked via
    /// `docker exec jiji-proxy jiji-proxy route ...`, the same pattern
    /// `docker exec kamal-proxy kamal-proxy deploy ...` uses today.
    Route {
        #[command(subcommand)]
        command: RouteCommand,
    },
    /// Prints the running daemon's own version -- used by jiji-cli to
    /// reject an already-running jiji-proxy below its required minimum
    /// version (see `version_requirements` in the jiji-cli crate).
    Version {
        #[arg(long, default_value = admin::DEFAULT_SOCKET_PATH)]
        socket: PathBuf,
    },
    /// Talks to a running jiji-proxy over its admin socket for raw TCP
    /// (non-HTTP) routes -- see `crate::tcp_relay`. A sibling of `route`,
    /// not a mode of it: TCP routes are keyed by `--listen-port`, not
    /// `--host`/`--path-prefix`/`--tls`, which are HTTP-only concepts.
    TcpRoute {
        #[command(subcommand)]
        command: TcpRouteCommand,
    },
}

#[derive(Subcommand)]
enum RouteCommand {
    /// Adds or replaces the route for `--host`.
    Apply {
        #[arg(long)]
        host: String,
        /// `None`/omitted is the catch-all for `--host`; if set, only
        /// matches a request path starting with this prefix.
        #[arg(long)]
        path_prefix: Option<String>,
        /// The DNS server to periodically re-query for `--name` -- in
        /// production, the local jiji-agent's `.jiji` resolver address.
        #[arg(long)]
        dns_server: SocketAddr,
        /// The name to resolve -- in production, `{project}-{service}.jiji`.
        #[arg(long)]
        name: String,
        #[arg(long)]
        port: u16,
        #[arg(long, default_value_t = 5)]
        refresh_interval_secs: u64,
        /// Serve/maintain a TLS certificate for this host (see acme.rs).
        #[arg(long)]
        tls: bool,
        /// Enables active health-checking against this HTTP path; omit
        /// `--health-check-path` (but pass this flag) for a TCP-only check,
        /// or omit the whole group to rely on DNS-driven eviction only.
        #[arg(long)]
        health_check: bool,
        #[arg(long)]
        health_check_path: Option<String>,
        #[arg(long, default_value_t = 5)]
        health_check_interval_secs: u64,
        #[arg(long, default_value_t = 2)]
        health_check_timeout_secs: u64,
        #[arg(long, default_value_t = 1)]
        health_check_consecutive_success: usize,
        #[arg(long, default_value_t = 1)]
        health_check_consecutive_failure: usize,
        #[arg(long, default_value = admin::DEFAULT_SOCKET_PATH)]
        socket: PathBuf,
    },
    /// Removes the route for `--host`, if present.
    Remove {
        #[arg(long)]
        host: String,
        #[arg(long)]
        path_prefix: Option<String>,
        #[arg(long, default_value = admin::DEFAULT_SOCKET_PATH)]
        socket: PathBuf,
    },
    /// Prints the currently configured routes as JSON.
    List {
        #[arg(long, default_value = admin::DEFAULT_SOCKET_PATH)]
        socket: PathBuf,
    },
    /// Prints the current backend addresses and health for one exact route
    /// as JSON -- used by jiji-cli right after a deploy commits a candidate
    /// Active in the catalog, to confirm jiji-proxy has actually discovered
    /// and health-checked it (see admin.rs's `RouteStatus` doc comment).
    Status {
        #[arg(long)]
        host: String,
        #[arg(long)]
        path_prefix: Option<String>,
        #[arg(long, default_value = admin::DEFAULT_SOCKET_PATH)]
        socket: PathBuf,
    },
}

#[derive(Subcommand)]
enum TcpRouteCommand {
    /// Adds or replaces the raw TCP route for `--listen-port`.
    Apply {
        #[arg(long)]
        listen_port: u16,
        /// The DNS server to periodically re-query for `--name` -- in
        /// production, the local jiji-agent's `.jiji` resolver address.
        #[arg(long)]
        dns_server: SocketAddr,
        /// The name to resolve -- in production, `{project}-{service}.jiji`.
        #[arg(long)]
        name: String,
        #[arg(long)]
        port: u16,
        #[arg(long, default_value_t = 5)]
        refresh_interval_secs: u64,
        /// Enables active health-checking; omit `--health-check-path` (but
        /// pass this flag) for a TCP-only check, or omit the whole group to
        /// rely on DNS-driven eviction only.
        #[arg(long)]
        health_check: bool,
        #[arg(long)]
        health_check_path: Option<String>,
        #[arg(long, default_value_t = 5)]
        health_check_interval_secs: u64,
        #[arg(long, default_value_t = 2)]
        health_check_timeout_secs: u64,
        #[arg(long, default_value_t = 1)]
        health_check_consecutive_success: usize,
        #[arg(long, default_value_t = 1)]
        health_check_consecutive_failure: usize,
        #[arg(long, default_value = admin::DEFAULT_SOCKET_PATH)]
        socket: PathBuf,
    },
    /// Removes the route for `--listen-port`, if present.
    Remove {
        #[arg(long)]
        listen_port: u16,
        #[arg(long, default_value = admin::DEFAULT_SOCKET_PATH)]
        socket: PathBuf,
    },
    /// Prints the currently configured TCP routes as JSON.
    List {
        #[arg(long, default_value = admin::DEFAULT_SOCKET_PATH)]
        socket: PathBuf,
    },
    /// Prints the current backend addresses and health for one exact TCP
    /// route as JSON -- mirrors `route status`.
    Status {
        #[arg(long)]
        listen_port: u16,
        #[arg(long, default_value = admin::DEFAULT_SOCKET_PATH)]
        socket: PathBuf,
    },
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Run { config } => run(config),
        Command::Route { command } => route_command(command),
        Command::Version { socket } => version_command(socket),
        Command::TcpRoute { command } => tcp_route_command(command),
    }
}

fn run(config_path: PathBuf) -> anyhow::Result<()> {
    let config = Config::load(&config_path)?;
    tracing::info!(
        config = %config_path.display(),
        seed_routes = config.routes.len(),
        http_listen = %config.http_listen,
        https_listen = ?config.https_listen,
        admin_socket = %config.admin_socket.display(),
        "jiji-proxy starting"
    );

    let mut server = Server::new(None).map_err(|error| anyhow::anyhow!("{error}"))?;
    server.bootstrap();

    let manager = RouteManager::new(config.admin_socket.clone());
    let challenges = PendingChallenges::default();

    // A single tokio runtime, scoped to this call, applies the config's seed
    // routes before the admin socket (spawned inside the manager's own
    // BackgroundService::start, once Pingora's runtime takes over at
    // run_forever()) accepts any requests. LoadBalancer/Backends discovery
    // is async even for this synchronous startup path.
    let seed_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    for route in &config.routes {
        seed_runtime.block_on(manager.apply(
            route.host.to_ascii_lowercase(),
            route.path_prefix.clone(),
            route.discovery.dns_server,
            route.discovery.name.clone(),
            route.discovery.port,
            route.discovery.refresh_interval_secs,
            route.tls,
            route.healthcheck.clone().map(HealthCheckSpec::from),
        ))?;
        tracing::info!(host = %route.host, name = %route.discovery.name, "seed route applied");
    }
    for route in &config.tcp_routes {
        seed_runtime.block_on(manager.tcp_apply(
            route.listen_port,
            route.discovery.dns_server,
            route.discovery.name.clone(),
            route.discovery.port,
            route.discovery.refresh_interval_secs,
            route.healthcheck.clone().map(HealthCheckSpec::from),
        ))?;
        tracing::info!(listen_port = route.listen_port, name = %route.discovery.name, "seed tcp route applied");
    }
    drop(seed_runtime);

    let cert_store = if config.https_listen.is_some() {
        Some(CertStore::load(config.cert_dir.clone())?)
    } else {
        None
    };

    server.add_service(background_service("route-manager", manager.clone()));

    if let (Some(acme), Some(cert_store)) = (&config.acme, &cert_store) {
        let acme_config = jiji_proxy::acme::AcmeConfig {
            directory_url: acme.directory_url.clone(),
            contact_email: acme.contact_email.clone(),
            account_path: acme.account_path.clone(),
            renew_before: acme.renew_before(),
            check_interval: acme.check_interval(),
        };
        let acme_manager = AcmeManager::new(
            acme_config,
            cert_store.clone(),
            manager.clone(),
            challenges.clone(),
        );
        server.add_service(background_service("acme-manager", acme_manager));
        tracing::info!(directory = %acme.directory_url, "ACME certificate automation enabled");
    }

    let routes = Arc::new(manager);

    let mut tcp_relay_service = pingora::services::listening::Service::new(
        "tcp-relay".to_string(),
        JijiTcpProxy {
            routes: routes.clone(),
        },
    );
    tcp_relay_service.add_tcp(&format!("0.0.0.0:{TCP_RELAY_PORT}"));
    server.add_service(tcp_relay_service);

    let mut proxy_service =
        http_proxy_service(&server.configuration, JijiProxy { routes, challenges });
    proxy_service.add_tcp(&config.http_listen.to_string());

    if let (Some(https_listen), Some(cert_store)) = (config.https_listen, cert_store) {
        let resolver: TlsAcceptCallbacks = Box::new(DynamicCertResolver { certs: cert_store });
        let tls_settings = TlsSettings::with_callbacks(resolver)
            .map_err(|error| anyhow::anyhow!("failed to configure TLS listener: {error}"))?;
        proxy_service.add_tls_with_settings(&https_listen.to_string(), None, tls_settings);
        tracing::info!(listen = %https_listen, "TLS listener configured");
    }

    server.add_service(proxy_service);
    server.run_forever();
}

fn route_command(command: RouteCommand) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        match command {
            RouteCommand::Apply {
                host,
                path_prefix,
                dns_server,
                name,
                port,
                refresh_interval_secs,
                tls,
                health_check,
                health_check_path,
                health_check_interval_secs,
                health_check_timeout_secs,
                health_check_consecutive_success,
                health_check_consecutive_failure,
                socket,
            } => {
                let request = AdminRequest::RouteApply {
                    host: host.to_ascii_lowercase(),
                    path_prefix,
                    dns_server,
                    name,
                    port,
                    refresh_interval_secs,
                    tls,
                    health_check: health_check.then_some(HealthCheckRequest {
                        path: health_check_path,
                        interval_secs: health_check_interval_secs,
                        timeout_secs: health_check_timeout_secs,
                        consecutive_success: health_check_consecutive_success,
                        consecutive_failure: health_check_consecutive_failure,
                    }),
                };
                match admin::call(&socket, &request).await? {
                    AdminResponse::Ok => {
                        println!("route applied for host '{host}'");
                        Ok(())
                    }
                    AdminResponse::Error { message } => {
                        anyhow::bail!("jiji-proxy rejected the route: {message}")
                    }
                    AdminResponse::Routes { .. }
                    | AdminResponse::Status { .. }
                    | AdminResponse::Version { .. }
                    | AdminResponse::TcpRoutes { .. }
                    | AdminResponse::TcpStatus { .. } => {
                        anyhow::bail!("jiji-proxy returned an unexpected response to route apply")
                    }
                }
            }
            RouteCommand::Remove {
                host,
                path_prefix,
                socket,
            } => {
                let request = AdminRequest::RouteRemove {
                    host: host.to_ascii_lowercase(),
                    path_prefix,
                };
                match admin::call(&socket, &request).await? {
                    AdminResponse::Ok => {
                        println!("route removed for host '{host}'");
                        Ok(())
                    }
                    AdminResponse::Error { message } => {
                        anyhow::bail!("jiji-proxy rejected the route removal: {message}")
                    }
                    AdminResponse::Routes { .. }
                    | AdminResponse::Status { .. }
                    | AdminResponse::Version { .. }
                    | AdminResponse::TcpRoutes { .. }
                    | AdminResponse::TcpStatus { .. } => {
                        anyhow::bail!("jiji-proxy returned an unexpected response to route remove")
                    }
                }
            }
            RouteCommand::List { socket } => {
                match admin::call(&socket, &AdminRequest::RouteList).await? {
                    AdminResponse::Routes { routes } => {
                        println!("{}", serde_json::to_string_pretty(&routes)?);
                        Ok(())
                    }
                    AdminResponse::Error { message } => {
                        anyhow::bail!("jiji-proxy rejected the route list request: {message}")
                    }
                    AdminResponse::Ok
                    | AdminResponse::Status { .. }
                    | AdminResponse::Version { .. }
                    | AdminResponse::TcpRoutes { .. }
                    | AdminResponse::TcpStatus { .. } => {
                        anyhow::bail!("jiji-proxy returned an unexpected response to route list")
                    }
                }
            }
            RouteCommand::Status {
                host,
                path_prefix,
                socket,
            } => {
                let request = AdminRequest::RouteStatus {
                    host: host.to_ascii_lowercase(),
                    path_prefix,
                };
                match admin::call(&socket, &request).await? {
                    AdminResponse::Status {
                        route_exists,
                        backends,
                    } => {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "route_exists": route_exists,
                                "backends": backends,
                            }))?
                        );
                        Ok(())
                    }
                    AdminResponse::Error { message } => {
                        anyhow::bail!("jiji-proxy rejected the route status request: {message}")
                    }
                    AdminResponse::Ok
                    | AdminResponse::Routes { .. }
                    | AdminResponse::Version { .. }
                    | AdminResponse::TcpRoutes { .. }
                    | AdminResponse::TcpStatus { .. } => {
                        anyhow::bail!("jiji-proxy returned an unexpected response to route status")
                    }
                }
            }
        }
    })
}

fn version_command(socket: PathBuf) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        match admin::call(&socket, &AdminRequest::Version).await? {
            AdminResponse::Version { version } => {
                println!("{version}");
                Ok(())
            }
            AdminResponse::Error { message } => {
                anyhow::bail!("jiji-proxy rejected the version request: {message}")
            }
            AdminResponse::Ok
            | AdminResponse::Routes { .. }
            | AdminResponse::Status { .. }
            | AdminResponse::TcpRoutes { .. }
            | AdminResponse::TcpStatus { .. } => {
                anyhow::bail!("jiji-proxy returned an unexpected response to version")
            }
        }
    })
}

fn tcp_route_command(command: TcpRouteCommand) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        match command {
            TcpRouteCommand::Apply {
                listen_port,
                dns_server,
                name,
                port,
                refresh_interval_secs,
                health_check,
                health_check_path,
                health_check_interval_secs,
                health_check_timeout_secs,
                health_check_consecutive_success,
                health_check_consecutive_failure,
                socket,
            } => {
                let request = AdminRequest::TcpRouteApply {
                    listen_port,
                    dns_server,
                    name,
                    port,
                    refresh_interval_secs,
                    health_check: health_check.then_some(HealthCheckRequest {
                        path: health_check_path,
                        interval_secs: health_check_interval_secs,
                        timeout_secs: health_check_timeout_secs,
                        consecutive_success: health_check_consecutive_success,
                        consecutive_failure: health_check_consecutive_failure,
                    }),
                };
                match admin::call(&socket, &request).await? {
                    AdminResponse::Ok => {
                        println!("tcp route applied for listen_port {listen_port}");
                        Ok(())
                    }
                    AdminResponse::Error { message } => {
                        anyhow::bail!("jiji-proxy rejected the tcp route: {message}")
                    }
                    AdminResponse::Routes { .. }
                    | AdminResponse::Status { .. }
                    | AdminResponse::Version { .. }
                    | AdminResponse::TcpRoutes { .. }
                    | AdminResponse::TcpStatus { .. } => {
                        anyhow::bail!(
                            "jiji-proxy returned an unexpected response to tcp route apply"
                        )
                    }
                }
            }
            TcpRouteCommand::Remove {
                listen_port,
                socket,
            } => {
                let request = AdminRequest::TcpRouteRemove { listen_port };
                match admin::call(&socket, &request).await? {
                    AdminResponse::Ok => {
                        println!("tcp route removed for listen_port {listen_port}");
                        Ok(())
                    }
                    AdminResponse::Error { message } => {
                        anyhow::bail!("jiji-proxy rejected the tcp route removal: {message}")
                    }
                    AdminResponse::Routes { .. }
                    | AdminResponse::Status { .. }
                    | AdminResponse::Version { .. }
                    | AdminResponse::TcpRoutes { .. }
                    | AdminResponse::TcpStatus { .. } => {
                        anyhow::bail!(
                            "jiji-proxy returned an unexpected response to tcp route remove"
                        )
                    }
                }
            }
            TcpRouteCommand::List { socket } => {
                match admin::call(&socket, &AdminRequest::TcpRouteList).await? {
                    AdminResponse::TcpRoutes { routes } => {
                        println!("{}", serde_json::to_string_pretty(&routes)?);
                        Ok(())
                    }
                    AdminResponse::Error { message } => {
                        anyhow::bail!("jiji-proxy rejected the tcp route list request: {message}")
                    }
                    AdminResponse::Ok
                    | AdminResponse::Routes { .. }
                    | AdminResponse::Status { .. }
                    | AdminResponse::Version { .. }
                    | AdminResponse::TcpStatus { .. } => {
                        anyhow::bail!(
                            "jiji-proxy returned an unexpected response to tcp route list"
                        )
                    }
                }
            }
            TcpRouteCommand::Status {
                listen_port,
                socket,
            } => {
                let request = AdminRequest::TcpRouteStatus { listen_port };
                match admin::call(&socket, &request).await? {
                    AdminResponse::TcpStatus {
                        route_exists,
                        backends,
                    } => {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "route_exists": route_exists,
                                "backends": backends,
                            }))?
                        );
                        Ok(())
                    }
                    AdminResponse::Error { message } => {
                        anyhow::bail!("jiji-proxy rejected the tcp route status request: {message}")
                    }
                    AdminResponse::Ok
                    | AdminResponse::Routes { .. }
                    | AdminResponse::Status { .. }
                    | AdminResponse::Version { .. }
                    | AdminResponse::TcpRoutes { .. } => {
                        anyhow::bail!(
                            "jiji-proxy returned an unexpected response to tcp route status"
                        )
                    }
                }
            }
        }
    })
}
