//! ACME (RFC 8555) certificate automation via `instant-acme`, HTTP-01 only.
//!
//! DNS-01 is deliberately not implemented here: it's the right challenge
//! type once more than one `jiji-proxy` instance can answer for the same
//! hostname (see "Challenge type" in plans/jiji-proxy-design.md), but that
//! needs a specific DNS provider's API wired in, which nobody has chosen
//! yet. HTTP-01 is correct and sufficient for a single ingress host per
//! hostname, which is what this phase actually needs to prove.

use bytes::Bytes;
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client as HyperClient;
use hyper_util::rt::TokioExecutor;
use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, BodyWrapper, ChallengeType, HttpClient,
    Identifier, NewAccount, NewOrder, OrderStatus, RetryPolicy,
};
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use crate::cert_store::CertStore;
use crate::route_manager::RouteManager;

/// Pebble (the local ACME test CA used to validate this integration -- see
/// plans/jiji-proxy-design.md) strictly enforces RFC 8555 section 6.1's
/// User-Agent requirement and rejects any request missing one with a 400
/// `malformed` problem document. instant-acme's own `DefaultClient` never
/// sets one, so `Account::builder()`/`from_credentials` against Pebble fail
/// with a confusing "missing field newNonce" JSON error (parsing that 400
/// problem body as if it were the directory). This wraps the same
/// hyper-rustls client `DefaultClient` builds internally, adding the header
/// instant-acme omits. Real Let's Encrypt doesn't enforce this as strictly,
/// but there's no reason to depend on that leniency continuing.
struct UserAgentClient(HyperClient<HttpsConnector<HttpConnector>, BodyWrapper<Bytes>>);

const USER_AGENT: &str = concat!("jiji-proxy/", env!("CARGO_PKG_VERSION"));

impl UserAgentClient {
    fn try_new() -> anyhow::Result<Self> {
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_native_roots()
            .map_err(|error| anyhow::anyhow!("failed to load native TLS roots: {error}"))?
            .https_only()
            .enable_http1()
            .enable_http2()
            .build();
        Ok(Self(
            HyperClient::builder(TokioExecutor::new()).build(connector),
        ))
    }
}

impl HttpClient for UserAgentClient {
    fn request(
        &self,
        mut req: http::Request<BodyWrapper<Bytes>>,
    ) -> Pin<
        Box<dyn Future<Output = Result<instant_acme::BytesResponse, instant_acme::Error>> + Send>,
    > {
        req.headers_mut().insert(
            http::header::USER_AGENT,
            http::HeaderValue::from_static(USER_AGENT),
        );
        HttpClient::request(&self.0, req)
    }
}

pub struct AcmeConfig {
    pub directory_url: String,
    pub contact_email: Option<String>,
    pub account_path: PathBuf,
    pub renew_before: Duration,
    pub check_interval: Duration,
}

/// Shared with `JijiProxy::request_filter`, which serves an HTTP-01 response
/// directly for any `/.well-known/acme-challenge/{token}` request -- before
/// routing, since the ACME CA validates against the naked host:80 path, not
/// through jiji-proxy's own route table.
#[derive(Clone, Default)]
pub struct PendingChallenges {
    inner: Arc<RwLock<HashMap<String, String>>>,
}

impl PendingChallenges {
    pub fn insert(&self, token: String, key_authorization: String) {
        self.inner
            .write()
            .expect("pending challenges lock poisoned")
            .insert(token, key_authorization);
    }

    pub fn remove(&self, token: &str) {
        self.inner
            .write()
            .expect("pending challenges lock poisoned")
            .remove(token);
    }

    pub fn get(&self, token: &str) -> Option<String> {
        self.inner
            .read()
            .expect("pending challenges lock poisoned")
            .get(token)
            .cloned()
    }
}

pub struct AcmeManager {
    config: AcmeConfig,
    certs: CertStore,
    routes: RouteManager,
    challenges: PendingChallenges,
}

impl AcmeManager {
    pub fn new(
        config: AcmeConfig,
        certs: CertStore,
        routes: RouteManager,
        challenges: PendingChallenges,
    ) -> Self {
        Self {
            config,
            certs,
            routes,
            challenges,
        }
    }

    async fn account(&self) -> anyhow::Result<Account> {
        if let Ok(raw) = tokio::fs::read(&self.config.account_path).await {
            let credentials: AccountCredentials = serde_json::from_slice(&raw)?;
            return Ok(
                Account::builder_with_http(Box::new(UserAgentClient::try_new()?))
                    .from_credentials(credentials)
                    .await?,
            );
        }

        let contact = self
            .config
            .contact_email
            .as_ref()
            .map(|email| format!("mailto:{email}"));
        let contact_ref: Option<&str> = contact.as_deref();
        let contact_slice: &[&str] = match &contact_ref {
            Some(value) => std::slice::from_ref(value),
            None => &[],
        };
        let (account, credentials) =
            Account::builder_with_http(Box::new(UserAgentClient::try_new()?))
                .create(
                    &NewAccount {
                        contact: contact_slice,
                        terms_of_service_agreed: true,
                        only_return_existing: false,
                    },
                    self.config.directory_url.clone(),
                    None,
                )
                .await?;

        if let Some(parent) = self.config.account_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(
            &self.config.account_path,
            serde_json::to_vec_pretty(&credentials)?,
        )
        .await?;
        tracing::info!(path = %self.config.account_path.display(), "ACME account created");
        Ok(account)
    }

    async fn check_all(&self, account: &Account) {
        for host in self.routes.tls_hosts() {
            if !self.certs.needs_issuance(&host, self.config.renew_before) {
                continue;
            }
            tracing::info!(host = %host, "requesting certificate");
            match self.issue_for_host(account, &host).await {
                Ok(()) => tracing::info!(host = %host, "certificate issued"),
                Err(error) => {
                    tracing::warn!(%error, host = %host, "certificate issuance failed; will retry on the next check")
                }
            }
        }
    }

    async fn issue_for_host(&self, account: &Account, host: &str) -> anyhow::Result<()> {
        let identifiers = [Identifier::Dns(host.to_string())];
        let mut order = account.new_order(&NewOrder::new(&identifiers)).await?;

        let mut pending_tokens = Vec::new();
        {
            let mut authorizations = order.authorizations();
            while let Some(result) = authorizations.next().await {
                let mut authz = result?;
                match authz.status {
                    AuthorizationStatus::Valid => continue,
                    AuthorizationStatus::Pending => {}
                    other => anyhow::bail!("unexpected authorization status {other:?} for {host}"),
                }
                let mut challenge = authz
                    .challenge(ChallengeType::Http01)
                    .ok_or_else(|| anyhow::anyhow!("no http-01 challenge offered for {host}"))?;

                let token = challenge.token.clone();
                self.challenges.insert(
                    token.clone(),
                    challenge.key_authorization().as_str().to_string(),
                );
                pending_tokens.push(token);
                challenge.set_ready().await?;
            }
        }

        let result = self.finalize_order(&mut order, host).await;
        for token in pending_tokens {
            self.challenges.remove(&token);
        }
        result
    }

    async fn finalize_order(
        &self,
        order: &mut instant_acme::Order,
        host: &str,
    ) -> anyhow::Result<()> {
        let retry = RetryPolicy::new().timeout(Duration::from_secs(60));

        let status = order.poll_ready(&retry).await?;
        if status != OrderStatus::Ready {
            anyhow::bail!("order for {host} ended in status {status:?}");
        }

        let private_key_pem = order.finalize().await?;
        let cert_chain_pem = order.poll_certificate(&retry).await?;

        self.certs
            .store_acme_cert(host, &cert_chain_pem, &private_key_pem)?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl pingora::services::background::BackgroundService for AcmeManager {
    async fn start(&self, mut shutdown: pingora::server::ShutdownWatch) {
        let account = match self.account().await {
            Ok(account) => account,
            Err(error) => {
                tracing::error!(%error, "could not establish an ACME account; certificate automation is disabled for this run");
                return;
            }
        };

        let mut ticker = tokio::time::interval(self.config.check_interval);
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    self.check_all(&account).await;
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        return;
                    }
                }
            }
        }
    }
}
