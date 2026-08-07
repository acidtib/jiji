use async_trait::async_trait;
use bytes::Bytes;
use pingora::prelude::*;
use std::sync::Arc;

use crate::acme::PendingChallenges;
use crate::route_manager::RouteManager;

const ACME_CHALLENGE_PREFIX: &str = "/.well-known/acme-challenge/";

pub struct JijiProxy {
    pub routes: Arc<RouteManager>,
    pub challenges: PendingChallenges,
}

#[async_trait]
impl ProxyHttp for JijiProxy {
    type CTX = ();

    fn new_ctx(&self) {}

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
        _ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>> {
        let host = session
            .get_header("host")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        let lookup_host = host.split(':').next().unwrap_or(&host).to_ascii_lowercase();
        let path = session.req_header().uri.path().to_string();

        let lb = self.routes.lookup(&lookup_host, &path).ok_or_else(|| {
            Error::explain(
                ErrorType::HTTPStatus(404),
                format!("no jiji-proxy route configured for host '{host}' path '{path}'"),
            )
        })?;

        let backend = lb.select(host.as_bytes(), 256).ok_or_else(|| {
            Error::explain(
                ErrorType::HTTPStatus(502),
                format!("no healthy backend currently discovered for host '{host}'"),
            )
        })?;

        Ok(Box::new(HttpPeer::new(backend, false, host)))
    }
}
