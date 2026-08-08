//! Raw TCP (non-HTTP) proxying. Unlike the HTTP proxy service, a TCP route
//! has no Host header to route by, so it can't share one listener the way
//! HTTP routes multiplex host-keyed entries behind a single port. Since
//! `pingora_core::server::Server::run_forever()` consumes the server and
//! can never have a new listener registered afterward, giving every TCP
//! route its own dedicated listener would mean restarting jiji-proxy (the
//! one host-global, multi-tenant component) every time a route is added or
//! removed -- explicitly rejected as a design constraint. Instead, exactly
//! one internal listener (`TCP_RELAY_PORT`) is registered once at startup;
//! every configured TCP route's public port DNATs to this same internal
//! port (see `jiji_network::proxy_script::render_nftables`), and which
//! route a given connection actually belongs to is recovered from the
//! kernel's own connection-tracking table via `SO_ORIGINAL_DST` -- the
//! standard Linux transparent-proxy technique used by HAProxy, Envoy, and
//! others. This makes adding/removing a TCP route a pure in-memory table
//! update (`RouteManager::tcp_apply`/`tcp_remove`) plus one nftables line,
//! with no cap on the number of routes and no restart, ever.

use async_trait::async_trait;
use pingora::apps::ServerApp;
use pingora::protocols::Stream;
use pingora::server::ShutdownWatch;
use std::mem;
use std::net::SocketAddr;
use std::sync::Arc;

use crate::route_manager::RouteManager;

/// The one internal port the TCP relay listens on for the lifetime of the
/// process -- never changes regardless of how many TCP routes exist.
pub const TCP_RELAY_PORT: u16 = 39100;

// <linux/netfilter_ipv4.h>: SOL_IP is IPPROTO_IP (0), SO_ORIGINAL_DST is 80.
const SOL_IP: libc::c_int = 0;
const SO_ORIGINAL_DST: libc::c_int = 80;

/// Recovers a DNAT'd connection's pre-NAT destination (the public port the
/// client actually dialed) from the kernel's conntrack table. Verified
/// against a real nftables DNAT rule using jiji's own `ip daddr`-scoped
/// rule shape before this module was written (see the TCP proxying design
/// plan's spike step) -- IPv4 only, matching jiji's ingress today.
fn original_dst(fd: libc::c_int) -> std::io::Result<SocketAddr> {
    let mut addr: libc::sockaddr_in = unsafe { mem::zeroed() };
    let mut len = mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            fd,
            SOL_IP,
            SO_ORIGINAL_DST,
            &mut addr as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let ip = std::net::Ipv4Addr::from(u32::from_be(addr.sin_addr.s_addr));
    let port = u16::from_be(addr.sin_port);
    Ok(SocketAddr::from((ip, port)))
}

pub struct JijiTcpProxy {
    pub routes: Arc<RouteManager>,
}

#[async_trait]
impl ServerApp for JijiTcpProxy {
    async fn process_new(
        self: &Arc<Self>,
        mut session: Stream,
        _shutdown: &ShutdownWatch,
    ) -> Option<Stream> {
        let fd = session.id();
        let dst = match original_dst(fd) {
            Ok(dst) => dst,
            Err(error) => {
                tracing::warn!(%error, "tcp relay: failed to recover original destination; closing connection");
                return None;
            }
        };
        let listen_port = dst.port();

        let Some(lb) = self.routes.tcp_lookup(listen_port) else {
            tracing::warn!(
                listen_port,
                "tcp relay: no route configured for this port; closing connection"
            );
            return None;
        };

        let Some(backend) = lb.select(listen_port.to_string().as_bytes(), 256) else {
            tracing::warn!(
                listen_port,
                "tcp relay: no healthy backend currently discovered; closing connection"
            );
            return None;
        };

        let Some(backend_addr) = backend.addr.as_inet().copied() else {
            tracing::warn!(
                listen_port,
                "tcp relay: backend has no resolvable address; closing connection"
            );
            return None;
        };

        let mut upstream = match tokio::net::TcpStream::connect(backend_addr).await {
            Ok(stream) => stream,
            Err(error) => {
                tracing::warn!(%error, listen_port, %backend_addr, "tcp relay: failed to connect to backend; closing connection");
                return None;
            }
        };

        if let Err(error) = tokio::io::copy_bidirectional(&mut session, &mut upstream).await {
            tracing::debug!(%error, listen_port, "tcp relay: connection ended");
        }

        None
    }
}
