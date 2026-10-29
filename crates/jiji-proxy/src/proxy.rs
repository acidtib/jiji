use async_trait::async_trait;
use bytes::Bytes;
use pingora::prelude::*;
use std::net::IpAddr;
use std::sync::Arc;

use crate::acme::PendingChallenges;
use crate::route_manager::RouteManager;

const ACME_CHALLENGE_PREFIX: &str = "/.well-known/acme-challenge/";

pub struct JijiProxy {
    pub routes: Arc<RouteManager>,
    pub challenges: PendingChallenges,
}

#[derive(Default)]
pub struct RequestContext {
    forward_headers: bool,
}

#[async_trait]
impl ProxyHttp for JijiProxy {
    type CTX = RequestContext;

    fn new_ctx(&self) -> Self::CTX {
        RequestContext::default()
    }

    /// Answers ACME HTTP-01 challenge requests directly, before routing:
    /// the CA validates against the naked `host:80` path, not through a
    /// configured route, and a route matching that host may not even exist
    /// yet on a host's very first certificate. See acme.rs.
    async fn request_filter(&self, session: &mut Session, _ctx: &mut Self::CTX) -> Result<bool> {
        let path = session.req_header().uri.path();
        let Some(token) = path.strip_prefix(ACME_CHALLENGE_PREFIX) else {
            return Ok(false);
        };

        match self.challenges.get(token) {
            Some(key_authorization) => {
                let mut header = ResponseHeader::build(200, Some(1))?;
                header.insert_header("Content-Length", key_authorization.len().to_string())?;
                session
                    .write_response_header(Box::new(header), false)
                    .await?;
                session
                    .write_response_body(Some(Bytes::from(key_authorization)), true)
                    .await?;
            }
            None => {
                session.respond_error(404).await?;
            }
        }
        Ok(true)
    }

    async fn upstream_peer(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>> {
        let host = session
            .get_header("host")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        let lookup_host = host.split(':').next().unwrap_or(&host).to_ascii_lowercase();
        let path = session.req_header().uri.path().to_string();

        let route = self
            .routes
            .lookup_route(&lookup_host, &path)
            .ok_or_else(|| {
                Error::explain(
                    ErrorType::HTTPStatus(404),
                    format!("no jiji-proxy route configured for host '{host}' path '{path}'"),
                )
            })?;
        ctx.forward_headers = route.forward_headers;

        let backend = route.lb.select(host.as_bytes(), 256).ok_or_else(|| {
            Error::explain(
                ErrorType::HTTPStatus(502),
                format!("no healthy backend currently discovered for host '{host}'"),
            )
        })?;

        Ok(Box::new(HttpPeer::new(backend, false, host)))
    }

    async fn upstream_request_filter(
        &self,
        session: &mut Session,
        upstream_request: &mut RequestHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()> {
        let client_ip = session
            .client_addr()
            .and_then(|address| address.as_inet())
            .map(|address| address.ip());
        let tls = session
            .digest()
            .is_some_and(|digest| digest.ssl_digest.is_some());
        set_forwarded_headers(upstream_request, client_ip, tls, ctx.forward_headers)
    }
}

/// Backends rate-limit and log by the right-most `X-Forwarded-For` entry, so
/// it is always the address that actually connected here. Without
/// `forward_headers`, client-supplied values are dropped entirely.
fn set_forwarded_headers(
    request: &mut RequestHeader,
    client_ip: Option<IpAddr>,
    tls: bool,
    forward_headers: bool,
) -> Result<()> {
    let incoming_for: Vec<String> = request
        .headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .map(str::to_string)
        .collect();
    let incoming_proto = header_value(request, "x-forwarded-proto");
    let incoming_host = header_value(request, "x-forwarded-host");
    for name in ["x-forwarded-for", "x-forwarded-proto", "x-forwarded-host"] {
        request.remove_header(name);
    }

    let mut chain = if forward_headers {
        incoming_for
    } else {
        Vec::new()
    };
    if let Some(ip) = client_ip {
        chain.push(ip.to_string());
    }
    if !chain.is_empty() {
        request.insert_header("x-forwarded-for", chain.join(", "))?;
    }

    let proto = incoming_proto
        .filter(|_| forward_headers)
        .unwrap_or_else(|| if tls { "https" } else { "http" }.to_string());
    request.insert_header("x-forwarded-proto", proto)?;

    let host = incoming_host
        .filter(|_| forward_headers)
        .or_else(|| header_value(request, "host"))
        .or_else(|| request.uri.host().map(str::to_string));
    if let Some(host) = host {
        request.insert_header("x-forwarded-host", host)?;
    }
    Ok(())
}

fn header_value(request: &RequestHeader, name: &str) -> Option<String> {
    request
        .headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod forwarded_header_tests {
    use super::*;

    const PEER: &str = "198.51.100.7";

    fn request(headers: &[(&str, &str)]) -> RequestHeader {
        let mut request = RequestHeader::build("GET", b"/api/health", None).unwrap();
        request.insert_header("host", "app.example.com").unwrap();
        for (name, value) in headers {
            request.append_header(name.to_string(), *value).unwrap();
        }
        request
    }

    fn header(request: &RequestHeader, name: &str) -> Vec<String> {
        request
            .headers
            .get_all(name)
            .iter()
            .map(|value| value.to_str().unwrap().to_string())
            .collect()
    }

    fn peer() -> Option<IpAddr> {
        Some(PEER.parse().unwrap())
    }

    #[test]
    fn spoofed_forwarded_for_is_replaced_by_the_peer_address() {
        let mut request = request(&[
            ("x-forwarded-for", "203.0.113.77"),
            ("x-forwarded-for", "203.0.113.78, 203.0.113.79"),
            ("x-forwarded-proto", "https"),
            ("x-forwarded-host", "evil.example.com"),
        ]);
        set_forwarded_headers(&mut request, peer(), false, false).unwrap();
        assert_eq!(header(&request, "x-forwarded-for"), vec![PEER]);
        assert_eq!(header(&request, "x-forwarded-proto"), vec!["http"]);
        assert_eq!(
            header(&request, "x-forwarded-host"),
            vec!["app.example.com"]
        );
    }

    #[test]
    fn request_without_forwarded_for_gets_the_peer_address() {
        let mut request = request(&[]);
        set_forwarded_headers(&mut request, peer(), true, false).unwrap();
        assert_eq!(header(&request, "x-forwarded-for"), vec![PEER]);
        assert_eq!(header(&request, "x-forwarded-proto"), vec!["https"]);
        assert_eq!(
            header(&request, "x-forwarded-host"),
            vec!["app.example.com"]
        );
    }

    #[test]
    fn forward_headers_appends_the_peer_address_as_the_last_entry() {
        let mut request = request(&[
            ("x-forwarded-for", "203.0.113.77"),
            ("x-forwarded-for", "203.0.113.88"),
            ("x-forwarded-proto", "https"),
            ("x-forwarded-host", "public.example.com"),
        ]);
        set_forwarded_headers(&mut request, peer(), false, true).unwrap();
        assert_eq!(
            header(&request, "x-forwarded-for"),
            vec![format!("203.0.113.77, 203.0.113.88, {PEER}")]
        );
        assert_eq!(header(&request, "x-forwarded-proto"), vec!["https"]);
        assert_eq!(
            header(&request, "x-forwarded-host"),
            vec!["public.example.com"]
        );
    }

    #[test]
    fn forward_headers_without_incoming_values_sets_its_own() {
        let mut request = request(&[]);
        set_forwarded_headers(&mut request, peer(), true, true).unwrap();
        assert_eq!(header(&request, "x-forwarded-for"), vec![PEER]);
        assert_eq!(header(&request, "x-forwarded-proto"), vec!["https"]);
        assert_eq!(
            header(&request, "x-forwarded-host"),
            vec!["app.example.com"]
        );
    }

    #[test]
    fn ipv6_peer_is_written_without_brackets_or_port() {
        let mut request = request(&[]);
        set_forwarded_headers(
            &mut request,
            Some("2001:db8::1".parse().unwrap()),
            false,
            false,
        )
        .unwrap();
        assert_eq!(header(&request, "x-forwarded-for"), vec!["2001:db8::1"]);
    }

    #[test]
    fn unknown_peer_still_drops_a_spoofed_forwarded_for() {
        let mut request = request(&[("x-forwarded-for", "203.0.113.77")]);
        set_forwarded_headers(&mut request, None, false, false).unwrap();
        assert!(header(&request, "x-forwarded-for").is_empty());
    }
}

#[cfg(test)]
mod socket_compatibility_tests {
    #[tokio::test]
    async fn pingora_can_wrap_an_accepted_tcp_connection() {
        // Guards the workspace Tokio pin: 1.54 rejects the zero-capacity buffer Pingora 0.9
        // creates here, panicking on every real HTTP connection despite route tests passing.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (client, server) =
            tokio::join!(tokio::net::TcpStream::connect(address), listener.accept());
        let _client = client.unwrap();
        let (socket, _) = server.unwrap();
        let _stream = pingora::protocols::l4::stream::Stream::from(socket);
    }
}
