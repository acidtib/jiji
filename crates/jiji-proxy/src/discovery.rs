use async_trait::async_trait;
use hickory_resolver::config::{NameServerConfig, ResolverConfig};
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::{Resolver, TokioResolver};
use pingora::lb::discovery::ServiceDiscovery;
use pingora::lb::Backend;
use pingora::prelude::{Error, ErrorType, Result};
use std::collections::{BTreeSet, HashMap};
use std::net::SocketAddr;

/// Resolves backends by periodically re-querying a specific DNS server (in
/// production, the local jiji-agent's `.jiji` resolver) for a service name,
/// rather than the host's system resolver. This is what lets a route's
/// backend set track jiji-agent's replicated catalog (mesh-wide, not just
/// this host) without jiji-cli ever pushing a per-deployment route update --
/// see `docs/architecture-notes.md#private-networking-wireguard-mesh--agent-served-dns`.
pub struct JijiDnsDiscovery {
    resolver: TokioResolver,
    name: String,
    port: u16,
}

impl JijiDnsDiscovery {
    pub fn new(dns_server: SocketAddr, name: String, port: u16) -> anyhow::Result<Self> {
        // `NameServerConfig`/`ConnectionConfig` are #[non_exhaustive], so the
        // port (which defaults to 53 via `udp()`) has to be overridden on
        // the built value rather than in a struct literal.
        let mut name_server = NameServerConfig::udp(dns_server.ip());
        for connection in &mut name_server.connections {
            connection.port = dns_server.port();
        }
        let config = ResolverConfig::from_parts(None, vec![], vec![name_server]);
        let resolver = Resolver::builder_with_config(config, TokioRuntimeProvider::default())
            .build()
            .map_err(|error| {
                anyhow::anyhow!(
                    "failed to build DNS resolver for '{name}' at {dns_server}: {error}"
                )
            })?;
        Ok(Self {
            resolver,
            name,
            port,
        })
    }
}

#[async_trait]
impl ServiceDiscovery for JijiDnsDiscovery {
    async fn discover(&self) -> Result<(BTreeSet<Backend>, HashMap<u64, bool>)> {
        let lookup = self
            .resolver
            .lookup_ip(self.name.as_str())
            .await
            .map_err(|error| {
                Error::because(
                    ErrorType::Custom("jiji-proxy dns discovery"),
                    format!("failed to resolve '{}'", self.name),
                    error,
                )
            })?;

        let mut backends = BTreeSet::new();
        for ip in lookup.iter() {
            backends.insert(Backend::new_with_weight(&format!("{ip}:{}", self.port), 1)?);
        }
        Ok((backends, HashMap::new()))
    }
}
