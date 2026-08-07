use async_trait::async_trait;
use pingora::lb::health_check::{HealthCheck, HttpHealthCheck, TcpHealthCheck};
use pingora::lb::selection::RoundRobin;
use pingora::lb::{Backends, LoadBalancer};
use pingora::prelude::RequestHeader;
use pingora::server::ShutdownWatch;
use pingora::services::background::BackgroundService;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::discovery::JijiDnsDiscovery;

/// Active health-checking, run on jiji-proxy's own schedule independent of
/// DNS re-resolution -- gives a backend that starts failing mid-interval
/// fast eviction from `select()` rather than waiting out
/// `refresh_interval_secs`. Feature-parity requirement for the kamal-proxy
/// cutover (kamal-proxy checks every route continuously; jiji-proxy
/// previously only trusted DNS-driven active/healthy filtering, per the
/// "Open questions" section of plans/jiji-proxy-design.md). `path: None`
/// checks TCP connectivity only, matching kamal-proxy's own fallback when no
/// `healthcheck.path`/`cmd` is configured. There is deliberately no `cmd`
/// variant: once routing is DNS-discovered mesh-wide instead of
/// local-only, execing into a container only works when the backend
/// happens to be on the same host as jiji-proxy itself, which cmd checks
/// can no longer assume -- jiji's own pre-activation gate (`health_check.rs`
/// in jiji-cli) remains the right place for `cmd` checks.
#[derive(Clone)]
pub struct HealthCheckSpec {
    pub path: Option<String>,
    pub interval: Duration,
    pub timeout: Duration,
    pub consecutive_success: usize,
    pub consecutive_failure: usize,
}

pub struct RouteEntry {
    pub lb: Arc<LoadBalancer<RoundRobin>>,
    pub dns_server: SocketAddr,
    pub name: String,
    pub port: u16,
    /// `None` is the catch-all for its host; `Some(prefix)` only matches a
    /// request path starting with `prefix`. Matches kamal-proxy's own
    /// "Host and Path Routing" semantics: longer prefixes take priority,
    /// and a route with no prefix is the fallback for anything a
    /// longer-prefixed sibling route on the same host doesn't claim.
    pub path_prefix: Option<String>,
    /// Whether this host should have a TLS certificate served/maintained
    /// for it (see acme.rs) -- `AcmeManager` derives its worklist from
    /// whichever hosts currently have this set, so a host stops being
    /// renewed automatically once its route is removed or reapplied
    /// without it. TLS is a connection-level (SNI), not path-level,
    /// property: if sibling routes on the same host disagree, any `true`
    /// wins (see `tls_hosts`).
    pub tls: bool,
    refresh_interval: Duration,
    last_refresh_millis: AtomicU64,
    health_check_interval: Option<Duration>,
    last_health_check_millis: AtomicU64,
}

/// A raw TCP route, keyed by its public `listen_port` (see `RouteManager::
/// tcp_routes`). There is no Host header to route by, so unlike `RouteEntry`
/// this carries no `path_prefix`/`tls` -- TLS termination for a raw TCP
/// protocol is out of scope for v1 (an encrypted backend protocol passes
/// through the relay opaquely, unchanged, since it never inspects payload
/// bytes). See `crate::tcp_relay` for the listener that dispatches into
/// this table via `SO_ORIGINAL_DST`.
pub struct TcpRouteEntry {
    pub lb: Arc<LoadBalancer<RoundRobin>>,
    pub dns_server: SocketAddr,
    pub name: String,
    pub port: u16,
    pub listen_port: u16,
    refresh_interval: Duration,
    last_refresh_millis: AtomicU64,
    health_check_interval: Option<Duration>,
    last_health_check_millis: AtomicU64,
}

/// Owns the mutable route table and is the single `BackgroundService`
/// registered with the Pingora `Server` for both admin-socket handling and
/// per-route DNS refresh -- Pingora only accepts new services before
/// `run_forever()` consumes it, so routes added later at runtime (via the
/// admin socket) can never be individually registered as their own
/// background service the way phase 2's static routes were. Driving one
/// shared tick loop over a table this struct owns is what makes route
/// apply/remove dynamic without a restart. See "Core design decision" and
/// "Control surface" in plans/jiji-proxy-design.md.
#[derive(Clone)]
pub struct RouteManager {
    /// Keyed by host; each host's entries are kept sorted by path_prefix
    /// length descending (the `None`/catch-all entry, if any, always last),
    /// so `lookup`'s first match is always the longest-prefix match.
    routes: Arc<RwLock<HashMap<String, Vec<Arc<RouteEntry>>>>>,
    /// Keyed by public `listen_port` directly -- no Host header to
    /// multiplex by, so one port can only ever serve one route. See
    /// `TcpRouteEntry`.
    tcp_routes: Arc<RwLock<HashMap<u16, Arc<TcpRouteEntry>>>>,
    pub socket_path: Arc<PathBuf>,
    tick_interval: Duration,
}

impl RouteManager {
    pub fn new(socket_path: PathBuf) -> Self {
        Self {
            routes: Arc::new(RwLock::new(HashMap::new())),
            tcp_routes: Arc::new(RwLock::new(HashMap::new())),
            socket_path: Arc::new(socket_path),
            tick_interval: Duration::from_secs(1),
        }
    }

    /// Backend load balancer currently assigned to `listen_port`, if any --
    /// the dispatch step `crate::tcp_relay::JijiTcpProxy` calls after
    /// recovering the original destination port via `SO_ORIGINAL_DST`.
    pub fn tcp_lookup(&self, listen_port: u16) -> Option<Arc<LoadBalancer<RoundRobin>>> {
        self.tcp_routes
            .read()
            .expect("tcp route table lock poisoned")
            .get(&listen_port)
            .map(|entry| entry.lb.clone())
    }

    /// Inserts or replaces the TCP route for `listen_port`. A `listen_port`
    /// already claimed by a *different* `name` (a different project or
    /// service) is rejected outright -- each project's config validation
    /// already prevents a same-project collision (see
    /// `jiji_config::validation::validate_tcp_targets`), so a collision
    /// reaching here means two different projects on the same host picked
    /// the same public port, which jiji-proxy itself (the one component
    /// with the whole host's picture) is what actually catches it. Reapplying
    /// the same `(listen_port, name)` pair (e.g. an unchanged redeploy) is
    /// always allowed and simply replaces the entry in place, matching
    /// `apply`'s own idempotent-overwrite behavior for HTTP routes.
    #[allow(clippy::too_many_arguments)]
    pub async fn tcp_apply(
        &self,
        listen_port: u16,
        dns_server: SocketAddr,
        name: String,
        port: u16,
        refresh_interval_secs: u64,
        health_check: Option<HealthCheckSpec>,
    ) -> anyhow::Result<()> {
        {
            let routes = self
                .tcp_routes
                .read()
                .expect("tcp route table lock poisoned");
            if let Some(existing) = routes.get(&listen_port) {
                if existing.name != name {
                    anyhow::bail!(
                        "listen_port {listen_port} is already in use by '{}'; each TCP route needs its own public port",
                        existing.name
                    );
                }
            }
        }

        let discovery = JijiDnsDiscovery::new(dns_server, name.clone(), port)?;
        let mut lb = LoadBalancer::<RoundRobin>::from_backends(Backends::new(Box::new(discovery)));

        let health_check_interval = health_check.as_ref().map(|spec| spec.interval);
        if let Some(spec) = &health_check {
            lb.set_health_check(build_health_check(&name, spec));
        }
        let lb = Arc::new(lb);

        if let Err(error) = lb.update().await {
            tracing::warn!(%error, listen_port, %name, "initial tcp route discovery failed; will retry on the next scheduled refresh");
        }
        if health_check_interval.is_some() {
            lb.backends().run_health_check(false).await;
        }

        let entry = Arc::new(TcpRouteEntry {
            lb,
            dns_server,
            name,
            port,
            listen_port,
            refresh_interval: Duration::from_secs(refresh_interval_secs.max(1)),
            last_refresh_millis: AtomicU64::new(now_millis()),
            health_check_interval,
            last_health_check_millis: AtomicU64::new(now_millis()),
        });
        self.tcp_routes
            .write()
            .expect("tcp route table lock poisoned")
            .insert(listen_port, entry);
        Ok(())
    }

    /// Returns whether a route was actually removed.
    pub fn tcp_remove(&self, listen_port: u16) -> bool {
        self.tcp_routes
            .write()
            .expect("tcp route table lock poisoned")
            .remove(&listen_port)
            .is_some()
    }

    pub fn tcp_list(&self) -> Vec<(u16, SocketAddr, String, u16, bool)> {
        self.tcp_routes
            .read()
            .expect("tcp route table lock poisoned")
            .values()
            .map(|entry| {
                (
                    entry.listen_port,
                    entry.dns_server,
                    entry.name.clone(),
                    entry.port,
                    entry.health_check_interval.is_some(),
                )
            })
            .collect()
    }

    /// Mirrors `backend_status` for the TCP table, keyed by `listen_port`.
    pub fn tcp_backend_status(&self, listen_port: u16) -> Option<Vec<(String, bool)>> {
        let routes = self
            .tcp_routes
            .read()
            .expect("tcp route table lock poisoned");
        let entry = routes.get(&listen_port)?;
        let backends = entry.lb.backends();
        let all = backends.get_backend();
        Some(
            all.iter()
                .map(|backend| (backend.addr.to_string(), backends.ready(backend)))
                .collect(),
        )
    }

    /// Exact-match first; if nothing is registered for `host` literally,
    /// falls back to whatever wildcard route would match it (see
    /// `wildcard::parent_wildcard_key` for the single-label matching rule).
    /// An exact-host route always wins over a wildcard for the same
    /// request, since the exact lookup is always tried first.
    pub fn lookup(&self, host: &str, path: &str) -> Option<Arc<LoadBalancer<RoundRobin>>> {
        let routes = self.routes.read().expect("route table lock poisoned");
        if let Some(entries) = routes.get(host) {
            if let Some(lb) = select_entry(entries, path) {
                return Some(lb);
            }
        }
        let wildcard_key = crate::wildcard::parent_wildcard_key(host)?;
        let entries = routes.get(&wildcard_key)?;
        select_entry(entries, path)
    }

    /// Inserts or replaces the route for `(host, path_prefix)`, always --
    /// even if the initial resolution attempt below fails or returns zero
    /// backends, both of which are legitimate transient states (DNS not up
    /// yet, service with no Active/Healthy replica yet), not reasons to
    /// refuse the route entirely. Only a malformed `dns_server`/`name`
    /// (which can't happen here since `dns_server` is already a parsed
    /// `SocketAddr`) fails this call; a failed or empty initial lookup is
    /// logged and left for the next scheduled refresh to retry.
    #[allow(clippy::too_many_arguments)]
    pub async fn apply(
        &self,
        host: String,
        path_prefix: Option<String>,
        dns_server: SocketAddr,
        name: String,
        port: u16,
        refresh_interval_secs: u64,
        tls: bool,
        health_check: Option<HealthCheckSpec>,
    ) -> anyhow::Result<()> {
        let discovery = JijiDnsDiscovery::new(dns_server, name.clone(), port)?;
        let mut lb = LoadBalancer::<RoundRobin>::from_backends(Backends::new(Box::new(discovery)));

        let health_check_interval = health_check.as_ref().map(|spec| spec.interval);
        if let Some(spec) = &health_check {
            lb.set_health_check(build_health_check(&name, spec));
        }
        let lb = Arc::new(lb);

        if let Err(error) = lb.update().await {
            tracing::warn!(%error, host = %host, %name, "initial route discovery failed; will retry on the next scheduled refresh");
        }
        if health_check_interval.is_some() {
            // Run once synchronously too: a freshly discovered backend
            // should not have to wait out a full health-check interval
            // before `select()` considers it ready for the first time.
            lb.backends().run_health_check(false).await;
        }

        let entry = Arc::new(RouteEntry {
            lb,
            dns_server,
            name,
            port,
            path_prefix: path_prefix.clone(),
            tls,
            refresh_interval: Duration::from_secs(refresh_interval_secs.max(1)),
            last_refresh_millis: AtomicU64::new(now_millis()),
            health_check_interval,
            last_health_check_millis: AtomicU64::new(now_millis()),
        });
        let mut routes = self.routes.write().expect("route table lock poisoned");
        let entries = routes.entry(host).or_default();
        entries.retain(|existing| existing.path_prefix != path_prefix);
        entries.push(entry);
        sort_by_prefix_length_descending(entries);
        Ok(())
    }

    /// Removes the specific `(host, path_prefix)` entry, not every route
    /// for `host` -- a host with sibling path-prefixed routes must keep
    /// the others. Returns whether an entry was actually removed.
    pub fn remove(&self, host: &str, path_prefix: Option<&str>) -> bool {
        let mut routes = self.routes.write().expect("route table lock poisoned");
        let Some(entries) = routes.get_mut(host) else {
            return false;
        };
        let before = entries.len();
        entries.retain(|entry| entry.path_prefix.as_deref() != path_prefix);
        let removed = entries.len() != before;
        if entries.is_empty() {
            routes.remove(host);
        }
        removed
    }

    #[allow(clippy::type_complexity)]
    pub fn list(&self) -> Vec<(String, Option<String>, SocketAddr, String, u16, bool, bool)> {
        self.routes
            .read()
            .expect("route table lock poisoned")
            .iter()
            .flat_map(|(host, entries)| {
                entries.iter().map(move |entry| {
                    (
                        host.clone(),
                        entry.path_prefix.clone(),
                        entry.dns_server,
                        entry.name.clone(),
                        entry.port,
                        entry.tls,
                        entry.health_check_interval.is_some(),
                    )
                })
            })
            .collect()
    }

    /// Current backend addresses for the exact `(host, path_prefix)` route,
    /// each paired with whether `select()` currently considers it ready --
    /// lets a caller (jiji-cli, after committing a deployment Active in the
    /// catalog) confirm a specific address has actually been discovered and
    /// passed health-checking, rather than trusting `apply`'s success alone.
    /// `None` means no such route exists. Matches `remove`'s exact-entry
    /// lookup, not `lookup`'s longest-prefix request-time matching.
    pub fn backend_status(
        &self,
        host: &str,
        path_prefix: Option<&str>,
    ) -> Option<Vec<(String, bool)>> {
        let routes = self.routes.read().expect("route table lock poisoned");
        let entries = routes.get(host)?;
        let entry = entries
            .iter()
            .find(|entry| entry.path_prefix.as_deref() == path_prefix)?;
        let backends = entry.lb.backends();
        let all = backends.get_backend();
        Some(
            all.iter()
                .map(|backend| (backend.addr.to_string(), backends.ready(backend)))
                .collect(),
        )
    }

    /// Hosts currently requesting a maintained TLS certificate --
    /// `AcmeManager`'s worklist on every check (see acme.rs). TLS is a
    /// connection-level property, so this is deduplicated by host: any
    /// path-prefixed route on a host requesting TLS is enough to issue a
    /// certificate for the whole host. Excludes wildcard-pattern hosts
    /// (`*.example.com`) unconditionally: jiji-proxy's ACME automation is
    /// HTTP-01 only, which cannot issue a wildcard certificate, so a
    /// wildcard route requesting `tls` here would only ever waste a Let's
    /// Encrypt attempt that's guaranteed to fail. Config validation already
    /// rejects `ssl: true` on a wildcard host before it ever reaches here;
    /// this filter is the same rule enforced independently at the admin
    /// socket, since that has no validation of its own.
    pub fn tls_hosts(&self) -> Vec<String> {
        self.routes
            .read()
            .expect("route table lock poisoned")
            .iter()
            .filter(|(host, entries)| {
                !crate::wildcard::is_wildcard_host(host) && entries.iter().any(|entry| entry.tls)
            })
            .map(|(host, _)| host.clone())
            .collect()
    }

    async fn refresh_due_routes(&self) {
        let snapshot: Vec<Arc<RouteEntry>> = self
            .routes
            .read()
            .expect("route table lock poisoned")
            .values()
            .flatten()
            .cloned()
            .collect();
        let now = now_millis();
        for entry in snapshot {
            let last = entry.last_refresh_millis.load(Ordering::Relaxed);
            if now.saturating_sub(last) >= entry.refresh_interval.as_millis() as u64 {
                if let Err(error) = entry.lb.update().await {
                    tracing::warn!(%error, name = %entry.name, "route discovery refresh failed");
                }
                entry.last_refresh_millis.store(now, Ordering::Relaxed);
            }

            if let Some(interval) = entry.health_check_interval {
                let last_check = entry.last_health_check_millis.load(Ordering::Relaxed);
                if now.saturating_sub(last_check) >= interval.as_millis() as u64 {
                    entry.lb.backends().run_health_check(false).await;
                    entry.last_health_check_millis.store(now, Ordering::Relaxed);
                }
            }
        }

        let tcp_snapshot: Vec<Arc<TcpRouteEntry>> = self
            .tcp_routes
            .read()
            .expect("tcp route table lock poisoned")
            .values()
            .cloned()
            .collect();
        for entry in tcp_snapshot {
            let last = entry.last_refresh_millis.load(Ordering::Relaxed);
            if now.saturating_sub(last) >= entry.refresh_interval.as_millis() as u64 {
                if let Err(error) = entry.lb.update().await {
                    tracing::warn!(%error, name = %entry.name, "tcp route discovery refresh failed");
                }
                entry.last_refresh_millis.store(now, Ordering::Relaxed);
            }

            if let Some(interval) = entry.health_check_interval {
                let last_check = entry.last_health_check_millis.load(Ordering::Relaxed);
                if now.saturating_sub(last_check) >= interval.as_millis() as u64 {
                    entry.lb.backends().run_health_check(false).await;
                    entry.last_health_check_millis.store(now, Ordering::Relaxed);
                }
            }
        }
    }
}

/// The first entry (already sorted longest-prefix-first, see
/// `sort_by_prefix_length_descending`) whose `path_prefix` matches `path`,
/// or the catch-all (`None`) entry if one exists and no sibling matched.
fn select_entry(entries: &[Arc<RouteEntry>], path: &str) -> Option<Arc<LoadBalancer<RoundRobin>>> {
    entries
        .iter()
        .find(|entry| match &entry.path_prefix {
            Some(prefix) => path.starts_with(prefix.as_str()),
            None => true,
        })
        .map(|entry| entry.lb.clone())
}

/// Longest prefix first; the catch-all (`None`) entry always sorts last,
/// since it must only ever be tried after every longer-prefixed sibling.
fn sort_by_prefix_length_descending(entries: &mut [Arc<RouteEntry>]) {
    entries.sort_by(|a, b| {
        let a_len = a.path_prefix.as_ref().map(String::len);
        let b_len = b.path_prefix.as_ref().map(String::len);
        match (a_len, b_len) {
            (Some(a), Some(b)) => b.cmp(&a),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
    });
}

fn build_health_check(
    name: &str,
    spec: &HealthCheckSpec,
) -> Box<dyn HealthCheck + Send + Sync + 'static> {
    match &spec.path {
        Some(path) => {
            let mut check = HttpHealthCheck::new(name, false);
            check.consecutive_success = spec.consecutive_success;
            check.consecutive_failure = spec.consecutive_failure;
            check.peer_template.options.connection_timeout = Some(spec.timeout);
            check.peer_template.options.read_timeout = Some(spec.timeout);
            match RequestHeader::build("GET", path.as_bytes(), None) {
                Ok(mut request) => {
                    if let Err(error) = request.append_header("Host", name) {
                        tracing::warn!(%error, name, "failed to set health check Host header");
                    }
                    check.req = request;
                }
                Err(error) => {
                    tracing::warn!(%error, name, %path, "invalid health check path; falling back to '/'");
                }
            }
            Box::new(check)
        }
        None => {
            let mut check = TcpHealthCheck::new();
            check.consecutive_success = spec.consecutive_success;
            check.consecutive_failure = spec.consecutive_failure;
            check.peer_template.options.connection_timeout = Some(spec.timeout);
            check
        }
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as u64
}

#[async_trait]
impl BackgroundService for RouteManager {
    async fn start(&self, mut shutdown: ShutdownWatch) {
        let admin_manager = self.clone();
        let admin_shutdown = shutdown.clone();
        tokio::spawn(async move {
            let socket_path = admin_manager.socket_path.clone();
            crate::admin::serve(&socket_path, admin_manager, admin_shutdown).await;
        });

        let mut ticker = tokio::time::interval(self.tick_interval);
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    self.refresh_due_routes().await;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path_prefix: Option<&str>) -> Arc<RouteEntry> {
        let discovery = JijiDnsDiscovery::new(
            "127.0.0.1:1".parse().unwrap(),
            "unused.jiji".to_string(),
            80,
        )
        .unwrap();
        let lb = Arc::new(LoadBalancer::<RoundRobin>::from_backends(Backends::new(
            Box::new(discovery),
        )));
        Arc::new(RouteEntry {
            lb,
            dns_server: "127.0.0.1:1".parse().unwrap(),
            name: "unused.jiji".to_string(),
            port: 80,
            path_prefix: path_prefix.map(str::to_string),
            tls: false,
            refresh_interval: Duration::from_secs(60),
            last_refresh_millis: AtomicU64::new(0),
            health_check_interval: None,
            last_health_check_millis: AtomicU64::new(0),
        })
    }

    #[test]
    fn sorting_puts_longest_prefix_first_and_catch_all_last() {
        let mut entries = vec![
            entry(None),
            entry(Some("/api")),
            entry(Some("/api/v2")),
            entry(Some("/a")),
        ];
        sort_by_prefix_length_descending(&mut entries);
        let order: Vec<Option<&str>> = entries
            .iter()
            .map(|entry| entry.path_prefix.as_deref())
            .collect();
        assert_eq!(order, vec![Some("/api/v2"), Some("/api"), Some("/a"), None]);
    }

    /// A deliberately unreachable DNS server: `apply` tolerates a failed
    /// initial resolution (see its own doc comment) and leaves the route
    /// registered for the next scheduled refresh to retry, so tests that
    /// only care about routing/matching, not actual backend discovery, can
    /// use this without a real DNS server.
    fn unreachable_dns_server() -> SocketAddr {
        "127.0.0.1:1".parse().unwrap()
    }

    async fn apply_route(
        manager: &RouteManager,
        host: &str,
        path_prefix: Option<&str>,
        name: &str,
    ) {
        manager
            .apply(
                host.to_string(),
                path_prefix.map(str::to_string),
                unreachable_dns_server(),
                name.to_string(),
                80,
                60,
                false,
                None,
            )
            .await
            .expect("apply tolerates unreachable DNS and always registers the route");
    }

    #[tokio::test]
    async fn wildcard_route_matches_a_single_label_subdomain() {
        let manager = RouteManager::new(PathBuf::from("/tmp/unused.sock"));
        apply_route(&manager, "*.example.com", None, "wild.jiji").await;
        assert!(manager.lookup("foo.example.com", "/").is_some());
        assert!(manager.lookup("bar.example.com", "/").is_some());
    }

    #[tokio::test]
    async fn wildcard_route_does_not_match_a_nested_subdomain() {
        let manager = RouteManager::new(PathBuf::from("/tmp/unused.sock"));
        apply_route(&manager, "*.example.com", None, "wild.jiji").await;
        assert!(manager.lookup("deep.foo.example.com", "/").is_none());
    }

    #[tokio::test]
    async fn wildcard_route_does_not_match_the_bare_parent_domain() {
        let manager = RouteManager::new(PathBuf::from("/tmp/unused.sock"));
        apply_route(&manager, "*.example.com", None, "wild.jiji").await;
        assert!(manager.lookup("example.com", "/").is_none());
    }

    #[tokio::test]
    async fn exact_host_route_takes_precedence_over_a_co_configured_wildcard() {
        let manager = RouteManager::new(PathBuf::from("/tmp/unused.sock"));
        apply_route(&manager, "*.example.com", None, "wild.jiji").await;
        apply_route(&manager, "api.example.com", None, "exact.jiji").await;

        let expected = manager
            .routes
            .read()
            .unwrap()
            .get("api.example.com")
            .unwrap()[0]
            .lb
            .clone();
        let matched = manager.lookup("api.example.com", "/").unwrap();
        assert!(
            Arc::ptr_eq(&expected, &matched),
            "the exact route's own load balancer should be the one returned, not the wildcard's"
        );
    }

    #[tokio::test]
    async fn a_more_specific_wildcard_level_does_not_fall_through_to_a_broader_one() {
        let manager = RouteManager::new(PathBuf::from("/tmp/unused.sock"));
        apply_route(&manager, "*.example.com", None, "general.jiji").await;
        apply_route(&manager, "*.staging.example.com", None, "staging.jiji").await;

        let expected = manager
            .routes
            .read()
            .unwrap()
            .get("*.staging.example.com")
            .unwrap()[0]
            .lb
            .clone();
        let matched = manager.lookup("foo.staging.example.com", "/").unwrap();
        assert!(
            Arc::ptr_eq(&expected, &matched),
            "the more specific *.staging.example.com wildcard should match, not *.example.com"
        );
    }

    #[tokio::test]
    async fn path_prefix_precedence_still_applies_within_a_wildcard_hosts_own_entries() {
        let manager = RouteManager::new(PathBuf::from("/tmp/unused.sock"));
        apply_route(&manager, "*.example.com", Some("/api"), "api.jiji").await;
        apply_route(&manager, "*.example.com", None, "catchall.jiji").await;

        let api_lb = manager
            .routes
            .read()
            .unwrap()
            .get("*.example.com")
            .unwrap()
            .iter()
            .find(|entry| entry.path_prefix.as_deref() == Some("/api"))
            .unwrap()
            .lb
            .clone();
        let catchall_lb = manager
            .routes
            .read()
            .unwrap()
            .get("*.example.com")
            .unwrap()
            .iter()
            .find(|entry| entry.path_prefix.is_none())
            .unwrap()
            .lb
            .clone();

        assert!(Arc::ptr_eq(
            &api_lb,
            &manager.lookup("foo.example.com", "/api/x").unwrap()
        ));
        assert!(Arc::ptr_eq(
            &catchall_lb,
            &manager.lookup("foo.example.com", "/other").unwrap()
        ));
    }

    #[tokio::test]
    async fn tls_hosts_excludes_wildcard_patterns_even_when_tls_requested() {
        let manager = RouteManager::new(PathBuf::from("/tmp/unused.sock"));
        manager
            .apply(
                "*.example.com".to_string(),
                None,
                unreachable_dns_server(),
                "wild.jiji".to_string(),
                80,
                60,
                true,
                None,
            )
            .await
            .unwrap();
        manager
            .apply(
                "exact.example.com".to_string(),
                None,
                unreachable_dns_server(),
                "exact.jiji".to_string(),
                80,
                60,
                true,
                None,
            )
            .await
            .unwrap();

        let tls_hosts = manager.tls_hosts();
        assert!(!tls_hosts.contains(&"*.example.com".to_string()));
        assert!(tls_hosts.contains(&"exact.example.com".to_string()));
    }

    async fn apply_tcp_route(manager: &RouteManager, listen_port: u16, name: &str) {
        manager
            .tcp_apply(
                listen_port,
                unreachable_dns_server(),
                name.to_string(),
                5432,
                60,
                None,
            )
            .await
            .expect("tcp_apply tolerates unreachable DNS and always registers the route");
    }

    #[tokio::test]
    async fn tcp_lookup_finds_a_route_by_its_listen_port() {
        let manager = RouteManager::new(PathBuf::from("/tmp/unused.sock"));
        apply_tcp_route(&manager, 5432, "db.jiji").await;
        assert!(manager.tcp_lookup(5432).is_some());
        assert!(manager.tcp_lookup(6379).is_none());
    }

    #[tokio::test]
    async fn tcp_remove_drops_the_route_and_reports_whether_one_existed() {
        let manager = RouteManager::new(PathBuf::from("/tmp/unused.sock"));
        apply_tcp_route(&manager, 5432, "db.jiji").await;
        assert!(manager.tcp_remove(5432));
        assert!(manager.tcp_lookup(5432).is_none());
        assert!(!manager.tcp_remove(5432), "already removed");
    }

    #[tokio::test]
    async fn tcp_apply_reapplying_the_same_name_replaces_in_place() {
        let manager = RouteManager::new(PathBuf::from("/tmp/unused.sock"));
        apply_tcp_route(&manager, 5432, "db.jiji").await;
        apply_tcp_route(&manager, 5432, "db.jiji").await;
        let list = manager.tcp_list();
        assert_eq!(
            list.len(),
            1,
            "reapplying the same route should not duplicate it"
        );
    }

    #[tokio::test]
    async fn tcp_apply_rejects_a_listen_port_already_claimed_by_a_different_name() {
        let manager = RouteManager::new(PathBuf::from("/tmp/unused.sock"));
        apply_tcp_route(&manager, 5432, "db.jiji").await;
        let result = manager
            .tcp_apply(
                5432,
                unreachable_dns_server(),
                "other-project-db.jiji".to_string(),
                5432,
                60,
                None,
            )
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("db.jiji"));
        // the original route must still be intact, not clobbered by the rejected apply
        let list = manager.tcp_list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].2, "db.jiji");
    }

    #[tokio::test]
    async fn tcp_list_and_backend_status_reflect_applied_routes() {
        let manager = RouteManager::new(PathBuf::from("/tmp/unused.sock"));
        apply_tcp_route(&manager, 5432, "db.jiji").await;
        let list = manager.tcp_list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].0, 5432);
        assert_eq!(list[0].2, "db.jiji");

        assert!(manager.tcp_backend_status(5432).is_some());
        assert!(manager.tcp_backend_status(9999).is_none());
    }
}
