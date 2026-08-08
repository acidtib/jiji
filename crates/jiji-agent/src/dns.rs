//! A minimal, hand-rolled authoritative DNS resolver serving exactly the `.jiji` zone this
//! project's catalog implies -- not a general-purpose resolver. Only standard queries (OPCODE 0),
//! class IN, and types A/ANY are meaningfully answered; anything else gets a structurally valid,
//! empty (NODATA) response rather than being treated as an error, matching how a real authoritative
//! server behaves for a record type it doesn't carry. This mirrors the rest of the agent's
//! philosophy of owning its own wire framing (`api.rs`, `replication.rs`) rather than reaching for
//! a general recursive-resolver library for a narrow, fully-controlled protocol surface.
//!
//! Serves both UDP (RFC 1035 message framing) and TCP (2-byte length-prefixed framing) on the same
//! bind address: a UDP answer that would not fit in 512 bytes is truncated to zero answers with the
//! TC bit set, and the client is expected to retry over TCP, where the full answer set is served
//! (bounded by [`MAX_ANSWERS`]).
//!
//! Per the plan's "Distributed DNS" section: only `active` + `healthy` catalog records are ever
//! answered with, and a record owned by a node this agent currently considers unreachable (see
//! `store.rs::node_liveness`) is suppressed -- reversibly, never deleted -- from both the aggregate
//! and per-server names.

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

use crate::catalog::{CatalogError, CatalogRecord, CatalogView};
use crate::membership::{MembershipError, MembershipScope, MembershipView, RecordProvenance};
use crate::store::{AgentStore, StoreError};

const QTYPE_A: u16 = 1;
const QTYPE_ANY: u16 = 255;
const QCLASS_IN: u16 = 1;
const RCODE_NOERROR: u8 = 0;
const RCODE_SERVFAIL: u8 = 2;
const RCODE_NOTIMP: u8 = 4;
const RCODE_NXDOMAIN: u8 = 3;
/// How long to wait for one forwarder's answer before trying the next configured one.
const FORWARD_TIMEOUT: Duration = Duration::from_secs(3);

pub const DEFAULT_TTL: u32 = 5;
/// A record set larger than this is truncated even over TCP; keeps a single answer within the
/// published capacity limits rather than growing unbounded (ADR 0007).
pub const MAX_ANSWERS: usize = 64;
/// The classic (non-EDNS0) UDP response ceiling. This resolver never advertises or honors a larger
/// EDNS0 buffer size -- a known simplification, see the Phase 4 handoff -- so any answer set that
/// wouldn't fit here always falls back to TCP even against a client offering a larger buffer.
const MAX_UDP_RESPONSE_BYTES: usize = 512;

#[derive(Debug, Error)]
pub enum DnsError {
    #[error("dns i/o failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("dns store failed: {0}")]
    Store(#[from] StoreError),
    #[error("dns membership check failed: {0}")]
    Membership(#[from] MembershipError),
    #[error("dns catalog check failed: {0}")]
    Catalog(#[from] CatalogError),
    #[error("agent store lock is poisoned")]
    LockPoisoned,
}

#[derive(Debug, Clone)]
pub struct DnsConfig {
    pub project_id: String,
    pub recovery_epoch: u64,
    pub local_node_id: String,
    /// A peer's replicas are suppressed once its last successful anti-entropy exchange is older
    /// than this. Reversible: the next successful exchange restores it (see `mark_node_seen`).
    pub reachability_timeout: Duration,
    /// Where a query outside this project's own `.jiji` zone is forwarded (see
    /// `handle_query`/`forward_query`) -- this resolver is the *only* nameserver a jiji-managed
    /// service container's `resolv.conf` ever gets, so without this, it could never resolve a
    /// normal internet hostname at all.
    pub forwarders: Vec<SocketAddr>,
}

impl DnsConfig {
    fn scope(&self) -> MembershipScope {
        MembershipScope::new(self.project_id.clone(), self.recovery_epoch)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedQuery {
    id: u16,
    opcode: u8,
    recursion_desired: bool,
    name: String,
    qtype: u16,
    qclass: u16,
    question_bytes: Vec<u8>,
}

fn parse_query(buf: &[u8]) -> Option<ParsedQuery> {
    if buf.len() < 12 {
        return None;
    }
    let id = u16::from_be_bytes([buf[0], buf[1]]);
    let flags1 = buf[2];
    let opcode = (flags1 >> 3) & 0x0F;
    let recursion_desired = flags1 & 0x01 != 0;
    let qdcount = u16::from_be_bytes([buf[4], buf[5]]);
    if qdcount != 1 {
        return None;
    }
    let question_start = 12;
    let mut offset = question_start;
    let mut labels = Vec::new();
    loop {
        let length = *buf.get(offset)? as usize;
        offset += 1;
        if length == 0 {
            break;
        }
        if length > 63 || labels.len() > 32 {
            return None;
        }
        let end = offset + length;
        let label = buf.get(offset..end)?;
        labels.push(String::from_utf8_lossy(label).to_ascii_lowercase());
        offset = end;
    }
    let qtype = u16::from_be_bytes(buf.get(offset..offset + 2)?.try_into().ok()?);
    offset += 2;
    let qclass = u16::from_be_bytes(buf.get(offset..offset + 2)?.try_into().ok()?);
    offset += 2;
    let question_bytes = buf.get(question_start..offset)?.to_vec();
    Some(ParsedQuery {
        id,
        opcode,
        recursion_desired,
        name: labels.join("."),
        qtype,
        qclass,
        question_bytes,
    })
}

fn encode_response(
    query: &ParsedQuery,
    rcode: u8,
    answers: &[Ipv4Addr],
    truncated: bool,
    authoritative: bool,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + query.question_bytes.len() + answers.len() * 16);
    out.extend_from_slice(&query.id.to_be_bytes());
    let flags1 = 0x80 // QR: response
        | (query.opcode << 3)
        | if authoritative { 0x04 } else { 0 } // AA: authoritative for the .jiji zone only
        | if truncated { 0x02 } else { 0 }
        | if query.recursion_desired { 0x01 } else { 0 };
    let flags2 = rcode & 0x0F;
    out.push(flags1);
    out.push(flags2);
    out.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
    out.extend_from_slice(&(answers.len() as u16).to_be_bytes()); // ANCOUNT
    out.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
    out.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT
    out.extend_from_slice(&query.question_bytes);
    for address in answers {
        out.extend_from_slice(&[0xC0, 0x0C]); // pointer back to the question's QNAME
        out.extend_from_slice(&QTYPE_A.to_be_bytes());
        out.extend_from_slice(&QCLASS_IN.to_be_bytes());
        out.extend_from_slice(&DEFAULT_TTL.to_be_bytes());
        out.extend_from_slice(&4u16.to_be_bytes()); // RDLENGTH
        out.extend_from_slice(&address.octets());
    }
    out
}

/// Answers one raw DNS message against `zone`. Returns `None` only when the input is too malformed
/// to safely echo back (can't even recover a query ID) -- matching how authoritative servers
/// typically just drop garbage rather than manufacture a response to it.
fn respond(buf: &[u8], zone: &BTreeMap<String, Vec<Ipv4Addr>>, is_udp: bool) -> Option<Vec<u8>> {
    let query = parse_query(buf)?;
    if query.opcode != 0 {
        return Some(encode_response(&query, RCODE_NOTIMP, &[], false, true));
    }
    if query.qclass != QCLASS_IN {
        return Some(encode_response(&query, RCODE_NOTIMP, &[], false, true));
    }
    let (rcode, mut answers) = match zone.get(&query.name) {
        None => (RCODE_NXDOMAIN, Vec::new()),
        Some(addresses) if query.qtype == QTYPE_A || query.qtype == QTYPE_ANY => {
            (RCODE_NOERROR, addresses.clone())
        }
        Some(_) => (RCODE_NOERROR, Vec::new()), // name exists, but not of the requested type
    };
    answers.truncate(MAX_ANSWERS);

    let full = encode_response(&query, rcode, &answers, false, true);
    if is_udp && full.len() > MAX_UDP_RESPONSE_BYTES {
        return Some(encode_response(&query, rcode, &[], true, true));
    }
    Some(full)
}

/// This resolver's own authority: exactly the suffix `zone_from_records` ever populates
/// (`{project}-{service}[-{server}].jiji`). Checked by name rather than zone membership so a
/// `.jiji` name this project genuinely doesn't have (typo, removed service) still gets a real
/// `NXDOMAIN` from `respond` -- an authoritative denial -- rather than being forwarded upstream,
/// where it would just as validly fail for an unrelated reason.
fn is_authoritative_name(name: &str) -> bool {
    name == "jiji" || name.ends_with(".jiji")
}

/// Forwards `query_bytes` verbatim to each of `forwarders` in turn (first answer wins). Raw bytes
/// in, raw bytes out: the response is relayed exactly as the forwarder sent it, never re-parsed or
/// re-encoded, since the query ID and question section reaching the forwarder are already
/// unchanged from what the original client sent. Each attempt gets its own ephemeral socket and a
/// bounded timeout so one unreachable forwarder can't block the others.
async fn forward_query(query_bytes: &[u8], forwarders: &[SocketAddr]) -> Option<Vec<u8>> {
    for forwarder in forwarders {
        let Ok(socket) = UdpSocket::bind("0.0.0.0:0").await else {
            continue;
        };
        if socket.connect(forwarder).await.is_err() {
            continue;
        }
        if socket.send(query_bytes).await.is_err() {
            continue;
        }
        let mut buf = [0u8; 4096];
        if let Ok(Ok(len)) = tokio::time::timeout(FORWARD_TIMEOUT, socket.recv(&mut buf)).await {
            return Some(buf[..len].to_vec());
        }
    }
    None
}

/// Top-level per-query dispatch used by the live UDP/TCP server loops (not by `respond`'s own
/// tests, which stay scoped to the authoritative-only path): answers a `.jiji` name locally via
/// `respond`, unchanged, or forwards anything else -- this resolver is the *only* nameserver a
/// jiji-managed service container's `resolv.conf` ever gets (see `service_runtime.rs`'s `--dns`
/// rendering), so without forwarding, a normal internet hostname could never resolve inside one of
/// these containers at all. Falls back to `SERVFAIL` (not `NXDOMAIN` -- we genuinely don't know,
/// we're not authoritatively denying) only if every configured forwarder is unreachable.
async fn handle_query(
    buf: &[u8],
    zone: &BTreeMap<String, Vec<Ipv4Addr>>,
    forwarders: &[SocketAddr],
    is_udp: bool,
) -> Option<Vec<u8>> {
    let query = parse_query(buf)?;
    if is_authoritative_name(&query.name) {
        return respond(buf, zone, is_udp);
    }
    if let Some(response) = forward_query(buf, forwarders).await {
        return Some(response);
    }
    Some(encode_response(&query, RCODE_SERVFAIL, &[], false, false))
}

/// Builds the served zone from currently active+healthy+reachable catalog records: an aggregate
/// name per service and a per-server name per (service, owning node) pair.
fn zone_from_records<'a>(
    project_id: &str,
    records: impl Iterator<Item = &'a CatalogRecord>,
    reachable: impl Fn(&str) -> bool,
) -> BTreeMap<String, Vec<Ipv4Addr>> {
    let mut zone: BTreeMap<String, Vec<Ipv4Addr>> = BTreeMap::new();
    for record in records {
        if !reachable(&record.owner_node_id) {
            continue;
        }
        zone.entry(format!("{project_id}-{}.jiji", record.service))
            .or_default()
            .push(record.address);
        zone.entry(format!(
            "{project_id}-{}-{}.jiji",
            record.service, record.owner_node_id
        ))
        .or_default()
        .push(record.address);
    }
    zone
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn build_zone(
    store: &AgentStore,
    config: &DnsConfig,
) -> Result<BTreeMap<String, Vec<Ipv4Addr>>, DnsError> {
    let scope = config.scope();
    let membership = MembershipView::from_records(store.membership_operations()?, &scope)?;
    // Already durably persisted, already checked at ingestion time -- rebuilding this view never
    // needs to re-authenticate anything, only reconstruct the CRDT (see `catalog.rs`'s module doc
    // comment on `RecordProvenance`).
    let catalog = CatalogView::from_records(
        store
            .catalog_operations()?
            .into_iter()
            .map(|record| (record, RecordProvenance::Verified)),
        &config.project_id,
        config.recovery_epoch,
        &membership,
    )?;
    let liveness = store.node_liveness()?;
    let now = unix_now();
    let timeout = config.reachability_timeout.as_secs();
    let reachable = |node_id: &str| -> bool {
        node_id == config.local_node_id
            || liveness
                .get(node_id)
                .is_some_and(|seen_at| now.saturating_sub(*seen_at) <= timeout)
    };
    Ok(zone_from_records(
        &config.project_id,
        catalog.active_healthy(),
        reachable,
    ))
}

fn locked_build_zone(
    store: &Arc<Mutex<AgentStore>>,
    config: &DnsConfig,
) -> Result<BTreeMap<String, Vec<Ipv4Addr>>, DnsError> {
    let store = store.lock().map_err(|_| DnsError::LockPoisoned)?;
    build_zone(&store, config)
}

async fn serve_udp(
    socket: UdpSocket,
    store: Arc<Mutex<AgentStore>>,
    config: DnsConfig,
) -> Result<(), DnsError> {
    // Spawned per packet (mirroring `serve_tcp`'s per-connection spawn) rather than handled
    // inline: forwarding a non-`.jiji` query is a real async wait on an external resolver (up to
    // `FORWARD_TIMEOUT` per configured forwarder), and this loop must not stall answering other
    // (including genuinely local `.jiji`) queries behind it.
    let socket = Arc::new(socket);
    let mut buf = [0u8; 4096];
    loop {
        let (len, peer) = socket.recv_from(&mut buf).await?;
        let query_bytes = buf[..len].to_vec();
        let socket = Arc::clone(&socket);
        let store = Arc::clone(&store);
        let config = config.clone();
        tokio::spawn(async move {
            let zone = match locked_build_zone(&store, &config) {
                Ok(zone) => zone,
                Err(error) => {
                    tracing::warn!(%error, "could not build dns zone for this query; skipping");
                    return;
                }
            };
            if let Some(response) =
                handle_query(&query_bytes, &zone, &config.forwarders, true).await
            {
                let _ = socket.send_to(&response, peer).await;
            }
        });
    }
}

async fn serve_tcp(
    listener: TcpListener,
    store: Arc<Mutex<AgentStore>>,
    config: DnsConfig,
) -> Result<(), DnsError> {
    loop {
        let (stream, _peer) = listener.accept().await?;
        let store = Arc::clone(&store);
        let config = config.clone();
        tokio::spawn(async move {
            if let Err(error) = serve_tcp_connection(stream, store, config).await {
                tracing::debug!(%error, "dns tcp connection ended");
            }
        });
    }
}

async fn serve_tcp_connection(
    mut stream: TcpStream,
    store: Arc<Mutex<AgentStore>>,
    config: DnsConfig,
) -> Result<(), DnsError> {
    loop {
        let mut length_bytes = [0u8; 2];
        if stream.read_exact(&mut length_bytes).await.is_err() {
            return Ok(());
        }
        let length = u16::from_be_bytes(length_bytes) as usize;
        let mut message = vec![0u8; length];
        stream.read_exact(&mut message).await?;
        let zone = locked_build_zone(&store, &config)?;
        let Some(response) = handle_query(&message, &zone, &config.forwarders, false).await else {
            return Ok(());
        };
        stream
            .write_all(&(response.len() as u16).to_be_bytes())
            .await?;
        stream.write_all(&response).await?;
    }
}

/// Binds and serves the `.jiji` zone on both UDP and TCP at `bind` until either fails.
pub async fn serve(
    bind: SocketAddr,
    store: Arc<Mutex<AgentStore>>,
    config: DnsConfig,
) -> Result<(), DnsError> {
    let udp_socket = UdpSocket::bind(bind).await?;
    let tcp_listener = TcpListener::bind(bind).await?;
    tokio::try_join!(
        serve_udp(udp_socket, Arc::clone(&store), config.clone()),
        serve_tcp(tcp_listener, store, config),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{DeploymentState, HealthState};

    fn build_query_bytes(id: u16, name: &str, qtype: u16) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&id.to_be_bytes());
        buf.push(0x01); // RD=1, opcode=0
        buf.push(0x00);
        buf.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        for label in name.split('.') {
            buf.push(label.len() as u8);
            buf.extend_from_slice(label.as_bytes());
        }
        buf.push(0);
        buf.extend_from_slice(&qtype.to_be_bytes());
        buf.extend_from_slice(&QCLASS_IN.to_be_bytes());
        buf
    }

    fn record(service: &str, node: &str, address: Ipv4Addr) -> CatalogRecord {
        CatalogRecord {
            project_id: "demo".into(),
            recovery_epoch: 1,
            protocol_version: crate::catalog::CATALOG_PROTOCOL_VERSION,
            schema_version: crate::catalog::CATALOG_SCHEMA_VERSION,
            service: service.into(),
            replica_id: format!("{node}-{service}"),
            owner_node_id: node.into(),
            owner_epoch: 1,
            revision: 1,
            deployment_id: "deploy-1".into(),
            address,
            ports: vec![80],
            image: "nginx:alpine".into(),
            state: DeploymentState::Active,
            health: HealthState::Healthy,
        }
    }

    #[test]
    fn parses_a_well_formed_query() {
        let bytes = build_query_bytes(42, "demo-web.jiji", QTYPE_A);
        let parsed = parse_query(&bytes).unwrap();
        assert_eq!(parsed.id, 42);
        assert_eq!(parsed.name, "demo-web.jiji");
        assert_eq!(parsed.qtype, QTYPE_A);
        assert!(parsed.recursion_desired);
    }

    #[test]
    fn a_short_or_garbage_buffer_is_dropped_not_answered() {
        assert!(respond(&[0u8; 3], &BTreeMap::new(), true).is_none());
    }

    #[test]
    fn known_name_resolves_and_unknown_name_is_nxdomain() {
        let mut zone = BTreeMap::new();
        zone.insert(
            "demo-web.jiji".to_string(),
            vec!["198.18.1.10".parse().unwrap()],
        );
        let hit = build_query_bytes(1, "demo-web.jiji", QTYPE_A);
        let response = respond(&hit, &zone, true).unwrap();
        assert_eq!(response[3] & 0x0F, RCODE_NOERROR);
        assert_eq!(u16::from_be_bytes([response[6], response[7]]), 1);

        let miss = build_query_bytes(1, "demo-missing.jiji", QTYPE_A);
        let response = respond(&miss, &zone, true).unwrap();
        assert_eq!(response[3] & 0x0F, RCODE_NXDOMAIN);
        assert_eq!(u16::from_be_bytes([response[6], response[7]]), 0);
    }

    #[test]
    fn a_known_name_with_an_unsupported_qtype_is_nodata_not_nxdomain() {
        let mut zone = BTreeMap::new();
        zone.insert(
            "demo-web.jiji".to_string(),
            vec!["198.18.1.10".parse().unwrap()],
        );
        let query = build_query_bytes(1, "demo-web.jiji", 28 /* AAAA */);
        let response = respond(&query, &zone, true).unwrap();
        assert_eq!(response[3] & 0x0F, RCODE_NOERROR);
        assert_eq!(u16::from_be_bytes([response[6], response[7]]), 0);
    }

    #[test]
    fn oversized_udp_answers_are_truncated_but_tcp_serves_them_in_full() {
        let addresses: Vec<Ipv4Addr> = (0..40).map(|i| Ipv4Addr::new(198, 18, 1, i)).collect();
        let mut zone = BTreeMap::new();
        zone.insert("demo-web.jiji".to_string(), addresses.clone());
        let query = build_query_bytes(1, "demo-web.jiji", QTYPE_A);

        let udp_response = respond(&query, &zone, true).unwrap();
        assert_eq!(udp_response[2] & 0x02, 0x02, "TC bit must be set");
        assert_eq!(u16::from_be_bytes([udp_response[6], udp_response[7]]), 0);

        let tcp_response = respond(&query, &zone, false).unwrap();
        assert_eq!(tcp_response[2] & 0x02, 0, "TCP response is never truncated");
        assert_eq!(
            u16::from_be_bytes([tcp_response[6], tcp_response[7]]),
            addresses.len() as u16
        );
    }

    #[test]
    fn zone_has_both_aggregate_and_per_server_names() {
        let records = [
            record("web", "node-a", "198.18.1.10".parse().unwrap()),
            record("web", "node-b", "198.18.2.10".parse().unwrap()),
        ];
        let zone = zone_from_records("demo", records.iter(), |_| true);
        assert_eq!(zone["demo-web.jiji"].len(), 2);
        assert_eq!(
            zone["demo-web-node-a.jiji"],
            vec!["198.18.1.10".parse::<Ipv4Addr>().unwrap()]
        );
        assert_eq!(
            zone["demo-web-node-b.jiji"],
            vec!["198.18.2.10".parse::<Ipv4Addr>().unwrap()]
        );
    }

    #[test]
    fn an_unreachable_nodes_replicas_are_suppressed_from_both_names() {
        let records = [
            record("web", "node-a", "198.18.1.10".parse().unwrap()),
            record("web", "node-b", "198.18.2.10".parse().unwrap()),
        ];
        let zone = zone_from_records("demo", records.iter(), |node| node != "node-b");
        assert_eq!(
            zone["demo-web.jiji"],
            vec!["198.18.1.10".parse::<Ipv4Addr>().unwrap()]
        );
        assert!(!zone.contains_key("demo-web-node-b.jiji"));
    }

    #[tokio::test]
    async fn serve_answers_real_udp_and_tcp_queries_end_to_end() {
        use crate::membership::{
            MembershipRecord, MembershipState, MEMBERSHIP_PROTOCOL_VERSION,
            MEMBERSHIP_SCHEMA_VERSION,
        };
        use tempfile::tempdir;

        let scope = MembershipScope::new("demo", 1);

        let dir = tempdir().unwrap();
        let store = AgentStore::open(&dir.path().join("agent.sqlite3")).unwrap();
        let membership_record = MembershipRecord {
            project_id: "demo".into(),
            recovery_epoch: 1,
            protocol_version: MEMBERSHIP_PROTOCOL_VERSION,
            schema_version: MEMBERSHIP_SCHEMA_VERSION,
            node_id: "node-a".into(),
            server_name: "node-a".into(),
            wireguard_public_key: "wg-a".into(),
            management_address: "100.98.64.1".parse().unwrap(),
            container_subnet: "198.18.1.0/24".into(),
            endpoints: vec!["192.0.2.1:51820".parse().unwrap()],
            owner_epoch: 1,
            revision: 1,
            state: MembershipState::Active,
        };
        store
            .apply_membership(membership_record.clone(), &scope)
            .unwrap();
        let mut membership_view = MembershipView::default();
        membership_view.apply(membership_record, &scope).unwrap();

        store
            .apply_catalog(
                record("web", "node-a", "198.18.1.10".parse().unwrap()),
                RecordProvenance::Local,
                "demo",
                1,
                &membership_view,
            )
            .unwrap();

        let store = Arc::new(Mutex::new(store));
        let config = DnsConfig {
            project_id: "demo".into(),
            recovery_epoch: 1,
            local_node_id: "node-a".into(),
            reachability_timeout: Duration::from_secs(3600),
            forwarders: Vec::new(),
        };

        let bind: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let udp_socket = UdpSocket::bind(bind).await.unwrap();
        let udp_address = udp_socket.local_addr().unwrap();
        let tcp_listener = TcpListener::bind(bind).await.unwrap();
        let tcp_address = tcp_listener.local_addr().unwrap();
        let udp_task = tokio::spawn(serve_udp(udp_socket, Arc::clone(&store), config.clone()));
        let tcp_task = tokio::spawn(serve_tcp(tcp_listener, store, config));

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client.connect(udp_address).await.unwrap();
        let query = build_query_bytes(7, "demo-web.jiji", QTYPE_A);
        client.send(&query).await.unwrap();
        let mut buf = [0u8; 512];
        let len = client.recv(&mut buf).await.unwrap();
        let response = &buf[..len];
        assert_eq!(response[3] & 0x0F, RCODE_NOERROR);
        assert_eq!(u16::from_be_bytes([response[6], response[7]]), 1);

        let mut tcp_stream = TcpStream::connect(tcp_address).await.unwrap();
        tcp_stream
            .write_all(&(query.len() as u16).to_be_bytes())
            .await
            .unwrap();
        tcp_stream.write_all(&query).await.unwrap();
        let mut length_bytes = [0u8; 2];
        tcp_stream.read_exact(&mut length_bytes).await.unwrap();
        let mut tcp_response = vec![0u8; u16::from_be_bytes(length_bytes) as usize];
        tcp_stream.read_exact(&mut tcp_response).await.unwrap();
        assert_eq!(tcp_response[3] & 0x0F, RCODE_NOERROR);
        assert_eq!(u16::from_be_bytes([tcp_response[6], tcp_response[7]]), 1);

        udp_task.abort();
        tcp_task.abort();
    }

    #[test]
    fn authoritative_name_check_matches_exactly_what_zone_from_records_ever_populates() {
        assert!(is_authoritative_name("demo-web.jiji"));
        assert!(is_authoritative_name("demo-web-app1.jiji"));
        assert!(is_authoritative_name("jiji"));
        assert!(!is_authoritative_name("api.themoviedb.org"));
        assert!(!is_authoritative_name("www.omdbapi.com"));
        // A name that merely ends with the letters "jiji" without a dot separator is not this
        // resolver's own zone.
        assert!(!is_authoritative_name("notjiji"));
    }

    /// A fake upstream resolver: replies to every query with a single canned A record, so tests
    /// can assert forwarding actually relays a real external answer rather than just "something
    /// came back."
    async fn spawn_fake_forwarder(answer: Ipv4Addr) -> (tokio::task::JoinHandle<()>, SocketAddr) {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = socket.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let mut buf = [0u8; 512];
            loop {
                let Ok((len, peer)) = socket.recv_from(&mut buf).await else {
                    return;
                };
                let Some(query) = parse_query(&buf[..len]) else {
                    continue;
                };
                let response = encode_response(&query, RCODE_NOERROR, &[answer], false, false);
                let _ = socket.send_to(&response, peer).await;
            }
        });
        (handle, address)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn forward_query_relays_a_real_upstream_answer_verbatim() {
        let (forwarder, forwarder_addr) =
            spawn_fake_forwarder("93.184.216.34".parse().unwrap()).await;

        let query = build_query_bytes(99, "api.themoviedb.org", QTYPE_A);
        let response = forward_query(&query, &[forwarder_addr]).await.unwrap();

        assert_eq!(u16::from_be_bytes([response[0], response[1]]), 99);
        assert_eq!(response[3] & 0x0F, RCODE_NOERROR);
        assert_eq!(u16::from_be_bytes([response[6], response[7]]), 1);
        assert!(response
            .windows(4)
            .any(|window| window == [93, 184, 216, 34]));

        forwarder.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn forward_query_falls_through_to_the_next_forwarder_when_the_first_is_unreachable() {
        let (forwarder, forwarder_addr) = spawn_fake_forwarder("1.2.3.4".parse().unwrap()).await;
        // Nothing listens here; the first attempt must time out/fail and fall through rather than
        // giving up after only the unreachable forwarder.
        let unreachable: SocketAddr = "127.0.0.1:1".parse().unwrap();

        let query = build_query_bytes(1, "example.com", QTYPE_A);
        let response = forward_query(&query, &[unreachable, forwarder_addr])
            .await
            .unwrap();
        assert!(response.windows(4).any(|window| window == [1, 2, 3, 4]));

        forwarder.abort();
    }

    #[tokio::test]
    async fn forward_query_returns_none_when_every_forwarder_is_unreachable() {
        let query = build_query_bytes(1, "example.com", QTYPE_A);
        // Port 0 is never a valid destination; this fails fast rather than actually waiting out
        // `FORWARD_TIMEOUT`, keeping the test quick.
        let nowhere: SocketAddr = "0.0.0.0:0".parse().unwrap();
        assert!(forward_query(&query, &[nowhere]).await.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn handle_query_answers_a_jiji_name_locally_and_forwards_everything_else() {
        let mut zone = BTreeMap::new();
        zone.insert(
            "demo-web.jiji".to_string(),
            vec!["198.18.1.10".parse().unwrap()],
        );
        let (forwarder, forwarder_addr) =
            spawn_fake_forwarder("203.0.113.9".parse().unwrap()).await;

        let local = build_query_bytes(1, "demo-web.jiji", QTYPE_A);
        let response = handle_query(&local, &zone, &[forwarder_addr], true)
            .await
            .unwrap();
        assert_eq!(response[3] & 0x0F, RCODE_NOERROR);
        assert!(response.windows(4).any(|window| window == [198, 18, 1, 10]));

        let external = build_query_bytes(2, "api.themoviedb.org", QTYPE_A);
        let response = handle_query(&external, &zone, &[forwarder_addr], true)
            .await
            .unwrap();
        assert!(response.windows(4).any(|window| window == [203, 0, 113, 9]));

        forwarder.abort();
    }

    #[tokio::test]
    async fn handle_query_returns_servfail_for_an_external_name_with_no_reachable_forwarder() {
        let zone = BTreeMap::new();
        let query = build_query_bytes(1, "api.themoviedb.org", QTYPE_A);
        let response = handle_query(&query, &zone, &[], true).await.unwrap();
        assert_eq!(response[3] & 0x0F, RCODE_SERVFAIL);
        // Never claim authority over a name we just forwarded (or tried to) -- AA must be unset.
        assert_eq!(response[2] & 0x04, 0);
    }
}
