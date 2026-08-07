//! Pure rendering/parsing for the shared, host-global jiji-proxy container and its Docker-only
//! nftables ingress DNAT workaround. Shared by `jiji-cli` (`proxy.rs`/`proxy_ingress.rs`, driven
//! over SSH) and `jiji-agent` (its own native reconciliation) so neither has to duplicate -- and
//! risk drifting from -- the other's understanding of jiji-proxy's run command, image
//! fingerprint, or DNAT rules.
//!
//! Replaces the kamal-proxy fork (see plans/jiji-proxy-design.md): jiji-proxy never execs into a
//! sibling container the way kamal-proxy's `--health-check-cmd` did, so unlike kamal-proxy's own
//! run command, this one needs no docker socket, no Podman toolchain bind-mount list, and no
//! `--privileged`/`--pid=host`/`--cgroupns=host`. It only needs network attachment (unchanged from
//! kamal-proxy) plus its own config file and certificate directory.

use std::net::Ipv4Addr;

use crate::BridgeEngineKind;

pub const CONTAINER_NAME: &str = "jiji-proxy";
pub const IMAGE: &str = "ghcr.io/acidtib/jiji-proxy:jiji";
pub const CERTS_DIR: &str = "/etc/jiji/certs";
/// Holds the rendered daemon config.yml (see `render_daemon_config`), mounted read-only into the
/// container. Distinct from `CERTS_DIR`: this directory has no per-host state, so it's safe to
/// re-render and overwrite on every `server setup`/reconcile tick.
pub const CONFIG_DIR: &str = "/etc/jiji/proxy";
pub const INTERNAL_HTTP_PORT: u16 = 8080;
pub const INTERNAL_HTTPS_PORT: u16 = 8443;
/// Must match `jiji_proxy::tcp_relay::TCP_RELAY_PORT` -- the one internal
/// port jiji-proxy's raw TCP relay listens on for the lifetime of the
/// process; every configured TCP route's public port DNATs to this same
/// port (see `render_nftables`), same duplicated-constant pattern already
/// used for `INTERNAL_HTTP_PORT`/`INTERNAL_HTTPS_PORT` above (jiji-network
/// doesn't depend on the jiji-proxy binary crate).
pub const INTERNAL_TCP_RELAY_PORT: u16 = 39100;
pub const INGRESS_TABLE: &str = "jiji_proxy_ingress";

/// The one project bridge jiji-proxy should be attached to as its *primary* network at creation
/// time (every other project's bridge is attached afterward via `network connect`, additive).
pub struct ProxyRunNetwork<'a> {
    pub bridge_name: &'a str,
    pub proxy_address: Ipv4Addr,
}

/// Identity used purely to decide "does the running container need replacing" (image/engine
/// drift) -- deliberately excludes any project's network address, since jiji-proxy can be
/// attached to several projects' bridges at once and none of them singularly identifies "the"
/// container's configuration anymore.
pub fn config_fingerprint(engine: BridgeEngineKind) -> String {
    format!("v1-{engine}")
}

/// jiji-proxy's own daemon config (see `crates/jiji-proxy/config.example.yml`) -- fixed,
/// convention-based, and identical for every project sharing a host, since jiji-proxy is the one
/// host-global, multi-tenant component (routes/TLS hosts are pushed per-project afterward via its
/// admin socket, never baked in here). ACME is always enabled against Let's Encrypt's production
/// directory with no contact email: `contact: &[]` is a valid ACME account per RFC 8555, and
/// there's no per-project value to put here anyway (one shared account for a host-global proxy).
/// Harmless for a host with no `tls: true` route: `AcmeManager`'s check loop simply finds no hosts
/// to issue for.
pub fn render_daemon_config() -> String {
    format!(
        "http_listen: \"0.0.0.0:{INTERNAL_HTTP_PORT}\"\n\
         https_listen: \"0.0.0.0:{INTERNAL_HTTPS_PORT}\"\n\
         cert_dir: {CERTS_DIR}\n\
         admin_socket: /run/jiji-proxy/admin.sock\n\
         acme:\n\
         \x20\x20directory_url: \"https://acme-v02.api.letsencrypt.org/directory\"\n"
    )
}

pub fn render_run_command(
    engine: BridgeEngineKind,
    network: Option<&ProxyRunNetwork<'_>>,
    fingerprint: &str,
) -> String {
    // `--network none` is a Docker/Podman-exclusive private mode: a container created with it can
    // never have a real network `connect`ed afterward (confirmed live, kamal-proxy era). So the
    // network that triggered this (re)creation must be attached right here, as the primary
    // network; every other project's bridge is added afterward via `network connect`, which works
    // fine once there's at least one real network already.
    let network_args = network.map_or_else(
        || " --network none".to_string(),
        |network| {
            format!(
                " --network {} --ip {}",
                network.bridge_name, network.proxy_address
            )
        },
    );

    format!(
        "{engine} run --name {CONTAINER_NAME}{network_args} --detach \
         --restart unless-stopped --label jiji.managed=true \
         --label jiji.proxy-config={fingerprint} \
         --volume {CERTS_DIR}:{CERTS_DIR} \
         --volume {CONFIG_DIR}:{CONFIG_DIR}:ro \
         --publish 80:{INTERNAL_HTTP_PORT} --publish 443:{INTERNAL_HTTPS_PORT} \
         {IMAGE}"
    )
}

/// Parses `{{json .NetworkSettings.Networks}}` output (an object keyed by network name, each
/// value carrying at least an `IPAddress` field) and returns the address attached to
/// `bridge_name`, if any.
pub fn attached_address(networks_json: &str, bridge_name: &str) -> Option<Ipv4Addr> {
    let value: serde_json::Value = serde_json::from_str(networks_json.trim()).ok()?;
    let address = value.get(bridge_name)?.get("IPAddress")?.as_str()?;
    if address.is_empty() {
        return None;
    }
    address.parse().ok()
}

pub fn is_missing_container_error(stderr: &str) -> bool {
    let stderr = stderr.to_ascii_lowercase();
    stderr.contains("no such container") || stderr.contains("no container with name or id")
}

/// DNATs the public HTTP ports (when `include_http` is set), plus every currently-configured TCP
/// route's public port (`tcp_ports`), to a single currently-attached jiji-proxy address. Never
/// needs to represent every co-resident project's routes: jiji-proxy is one process listening on
/// every attached interface, so any currently-attached address reaches the same process, which
/// does its own per-request routing (by Host header for HTTP, by `SO_ORIGINAL_DST` for raw TCP --
/// see `crate::tcp_relay` in the jiji-proxy crate) -- not by which address the DNAT rule happened
/// to name.
///
/// A TCP route's own public port is deliberately preserved here, not rewritten to
/// `INTERNAL_TCP_RELAY_PORT` the way an earlier version of this function did: `SO_ORIGINAL_DST`
/// only recovers a DNAT rewrite from the same network namespace the rewrite happened in, and
/// jiji-proxy runs inside its own container network namespace, separate from the host's (confirmed
/// live -- a rewrite applied here, in the host's namespace, comes back `ENOENT` when queried from
/// inside the container). This rule therefore only ever changes the *address* (routing the packet
/// to the right container at all); the port-to-`INTERNAL_TCP_RELAY_PORT` remap happens as a second,
/// in-container-netns-only NAT stage applied by jiji-agent via `nsenter`
/// (`render_relay_netns_apply_script`), so the rewrite `SO_ORIGINAL_DST` actually recovers happens
/// in the same namespace jiji-proxy itself queries from.
///
/// `include_http` exists because this whole DNAT workaround was originally Docker-only (see
/// `proxy_ingress.rs`'s module doc comment): Podman's bridges don't disable IP masquerade, so its
/// own `--publish 80:8080`/`--publish 443:8443` already works there without this table's help --
/// adding a second, independent DNAT rule for the exact same ports risks an unpredictable
/// interaction between two competing `prerouting` chains. A TCP route's public port, however, has
/// no such native alternative on *either* engine: `--publish` can only ever publish a fixed port
/// set declared at container-creation time, which is fundamentally incompatible with adding a new
/// TCP route without ever restarting jiji-proxy (see `tcp_relay.rs`'s own design constraint) -- so
/// `tcp_ports` is always included, on both engines, while the two static HTTP lines stay opt-in.
///
/// Every DNAT rule is restricted to `ip daddr {public_host}` -- this host's own public IP -- for
/// the same reason a `chain output` DNAT (present in an earlier version of this ruleset, matching
/// locally-*generated* packets by destination port alone) had to be removed entirely: with no
/// destination-address restriction, `prerouting` matches on destination *port* alone too, and
/// `prerouting` fires for every packet arriving on any interface before the routing decision --
/// including bridge-originated traffic merely passing through this host on its way to a *remote*
/// peer over WireGuard. Confirmed live (kamal-proxy era): a container calling a *different* host's
/// replica on port 80 got silently hijacked back to this host's own proxy instead of ever reaching
/// the real destination, because the rule matched on `tcp dport 80` with no regard for where the
/// packet was actually headed. Restricting to `ip daddr {public_host}` scopes every rule to its
/// only intended case: traffic genuinely arriving from the WAN, addressed to this host's own
/// public IP.
///
/// This table's own `forward`-hook chain was tried first for the "DNAT'd packet gets silently
/// dropped" problem described below, and confirmed live to be ineffective: an independently
/// registered nftables base chain's `accept` verdict does not override a *later*-priority chain's
/// own verdict at the same hook, regardless of numeric priority ordering -- Podman's own
/// vendor-managed `FORWARD` chain still evaluates (and drops) the packet afterward. See
/// `render_forward_accept_script` for the fix that actually works (inserting directly into the
/// engine's own `FORWARD` chain, the codebase's already-established pattern for this exact class
/// of problem -- see `bridge_script::render_restore_script`'s identical `ensure_rule` use for
/// WireGuard<->bridge forwarding).
pub fn render_nftables(
    address: Ipv4Addr,
    public_host: Ipv4Addr,
    include_http: bool,
    tcp_ports: &[u16],
) -> String {
    let mut rules = if include_http {
        format!(
            "\t\tip daddr {public_host} tcp dport 80 dnat to {address}:{INTERNAL_HTTP_PORT}\n\
         \t\tip daddr {public_host} tcp dport 443 dnat to {address}:{INTERNAL_HTTPS_PORT}\n"
        )
    } else {
        String::new()
    };
    for port in tcp_ports {
        rules.push_str(&format!(
            "\t\tip daddr {public_host} tcp dport {port} dnat to {address}:{port}\n"
        ));
    }
    format!(
        "delete table ip {INGRESS_TABLE}\n\
         table ip {INGRESS_TABLE} {{\n\
         \tchain prerouting {{\n\
         \t\ttype nat hook prerouting priority dstnat - 5; policy accept;\n\
         {rules}\
         \t}}\n\
         }}\n"
    )
}

/// Table name for the second-stage NAT applied *inside* jiji-proxy's own container network
/// namespace (see `render_relay_netns_apply_script`) -- distinct from `INGRESS_TABLE`, which lives
/// in the host's namespace, so the two can never collide or be confused for one another when
/// inspecting either namespace independently.
pub const RELAY_NAT_TABLE: &str = "jiji_tcp_relay";

/// Renders the in-container-netns NAT ruleset that remaps each TCP route's own public port to
/// jiji-proxy's fixed internal relay listener (`INTERNAL_TCP_RELAY_PORT`). This is the second half
/// of the two-stage DNAT described on `render_nftables`: the host-side rule only rewrites the
/// destination *address* (routing the packet to the right container), preserving the port, so that
/// this second rewrite -- applied from *inside* jiji-proxy's own namespace -- is the one whose
/// conntrack entry `SO_ORIGINAL_DST` actually recovers from within that same namespace.
///
/// Same shape as `render_nftables`'s own leading `delete table` -- a fresh, empty `tcp_ports` still
/// renders a `delete table` line (clearing any stale entries from routes that were since removed)
/// rather than nothing at all, keeping this namespace's table converged to exactly the current
/// route set on every call, the same "unconditionally re-render the whole table" pattern used
/// throughout this module.
fn render_relay_netns_nftables(tcp_ports: &[u16]) -> String {
    let mut rules = String::new();
    for port in tcp_ports {
        rules.push_str(&format!(
            "\t\ttcp dport {port} dnat to :{INTERNAL_TCP_RELAY_PORT}\n"
        ));
    }
    format!(
        "delete table ip {RELAY_NAT_TABLE}\n\
         table ip {RELAY_NAT_TABLE} {{\n\
         \tchain prerouting {{\n\
         \t\ttype nat hook prerouting priority dstnat - 5; policy accept;\n\
         {rules}\
         \t}}\n\
         }}\n"
    )
}

/// Renders a self-contained `sh` script (run the same way `render_forward_accept_script`'s output
/// is, via `bridge_bringup::run_script`'s "pipe the whole script over `sh -s`'s own stdin" pattern)
/// that applies `render_relay_netns_nftables`'s ruleset *inside* jiji-proxy's own container network
/// namespace via `nsenter`. `pid` is jiji-proxy's own container PID as seen from the host
/// (`{{.State.Pid}}`, i.e. the PID namespace-*host* view of the container's init process); the
/// ruleset is embedded as an inline heredoc rather than piped separately, since `sh -s` already
/// consumes the whole script from stdin -- a heredoc within that same script text is read from the
/// remainder of that identical stream, the standard way a shell script embeds inline data.
///
/// This needs no capabilities or tooling inside jiji-proxy's own container image or process:
/// `nsenter --net` only changes which network namespace the caller -- the host's own already
/// -privileged `nft` -- operates against; only the network namespace is entered (no `--mount`), so
/// the `nft` binary invoked is the host's own, and the namespace switch has no bearing on what's
/// installed inside the container. Pre-creating the table (ignoring failure) mirrors the host-side
/// ingress table's own idempotent bring-up, tolerating a cold boot where the table doesn't exist yet
/// and the ruleset's own leading `delete table` line would otherwise fail.
pub fn render_relay_netns_apply_script(pid: u32, tcp_ports: &[u16]) -> String {
    let ruleset = render_relay_netns_nftables(tcp_ports);
    format!(
        "nsenter --net=/proc/{pid}/ns/net -- nft add table ip {RELAY_NAT_TABLE} 2>/dev/null || true\n\
         nsenter --net=/proc/{pid}/ns/net -- nft -f - <<'JIJI_RELAY_NFT'\n\
         {ruleset}\
         JIJI_RELAY_NFT\n"
    )
}

/// Idempotently authorizes traffic to jiji-proxy's own bridge address in the container engine's
/// `FORWARD` chain (and `DOCKER-USER`, if present), mirroring
/// `bridge_script::render_restore_script`'s identical `ensure_rule` pattern for WireGuard<->bridge
/// forwarding -- the codebase's own established, already-working precedent for authorizing traffic
/// the engine's own vendor-managed firewall wouldn't otherwise expect.
///
/// A TCP route's own public port is opened here individually, not `INTERNAL_TCP_RELAY_PORT`:
/// `render_nftables`'s DNAT now preserves a TCP route's port (address-only rewrite), so a packet
/// reaching this hook still carries its original public port, not the shared relay port -- the
/// remap to `INTERNAL_TCP_RELAY_PORT` only happens later, inside jiji-proxy's own container
/// namespace (see `render_relay_netns_apply_script`), after this `FORWARD` hook.
///
/// Confirmed live this was actually needed: Podman's netavark backend installs a `policy drop`
/// `FORWARD` chain whose only allowances are `ct state related,established` for the container
/// subnet, or traffic sourced from the subnet/loopback itself (its own `--publish`
/// authorization bookkeeping, scoped to *local* or same-subnet callers). A genuinely external
/// WAN client's fresh connection -- exactly what `render_nftables`'s DNAT rewrite exists to
/// receive -- satisfies neither condition, so it reaches the `FORWARD` hook and is silently
/// dropped by Podman's own chain regardless of anything `jiji_proxy_ingress`'s own `prerouting`
/// chain decided; a same-hook, earlier-priority `accept` in a *different* table does not override
/// this (confirmed live, see `render_nftables`'s own doc comment). `iptables` (the classic CLI,
/// which both Docker's dockerd and Podman's netavark also manage their own `FORWARD` chain
/// through, via the iptables-nft compatibility layer) is used rather than a raw `nft insert rule
/// ip filter FORWARD ...`, matching the bridge/WireGuard precedent exactly rather than inventing a
/// second way to solve the same class of problem.
///
/// HTTP ports 8080/8443 are opened unconditionally, on both engines, not just Docker: an earlier
/// version of this function only opened them for `include_http` (Docker, since it's the one whose
/// own native `--publish` is broken on jiji's bridges and needs `render_nftables`'s own DNAT
/// workaround). Confirmed live this was wrong -- Podman's own *native* `--publish` DNAT for 80/443
/// (which does work, unlike Docker's) hits this exact same `FORWARD` hook and was silently dropped
/// for a genuinely external client the same way a TCP route's DNAT was, since the drop happens
/// after the DNAT regardless of which mechanism performed it. jiji-proxy always listens on both
/// internal HTTP ports regardless of route configuration, so opening them here is always correct.
pub fn render_forward_accept_script(address: Ipv4Addr, tcp_ports: &[u16]) -> String {
    let mut ports = vec![INTERNAL_HTTP_PORT, INTERNAL_HTTPS_PORT];
    ports.extend(tcp_ports.iter().copied());

    let forward_rules = ports
        .iter()
        .map(|port| format!("ensure_rule FORWARD -d {address} -p tcp --dport {port} -j ACCEPT"))
        .collect::<Vec<_>>()
        .join("\n");
    let docker_user_rules = ports
        .iter()
        .map(|port| {
            format!("  ensure_rule DOCKER-USER -d {address} -p tcp --dport {port} -j ACCEPT")
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "#!/bin/sh\n\
         set -eu\n\
         \n\
         ensure_rule() {{\n\
         \x20\x20if ! iptables -C \"$@\" 2>/dev/null; then\n\
         \x20\x20\x20\x20iptables -I \"$@\"\n\
         \x20\x20fi\n\
         }}\n\
         \n\
         {forward_rules}\n\
         if iptables -n -L DOCKER-USER >/dev/null 2>&1; then\n\
         {docker_user_rules}\n\
         fi\n"
    )
}

/// Finds a still-attached jiji bridge address from `{{range ...}}{{printf "%s %s\n" $name
/// $network.IPAddress}}{{end}}`-shaped inspect output, for recovering the ingress rule after
/// jiji-proxy survives but its ingress state was lost.
pub fn surviving_proxy_address(output: &str) -> Option<Ipv4Addr> {
    output.lines().find_map(|line| {
        let (network, address) = line.split_once(' ')?;
        network
            .starts_with("jiji-")
            .then(|| address.parse::<Ipv4Addr>().ok())
            .flatten()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_run_omits_network_when_none_given_and_needs_no_exec_privileges() {
        let command = render_run_command(BridgeEngineKind::Docker, None, "v1-docker");
        assert!(command.contains("ghcr.io/acidtib/jiji-proxy:jiji"));
        assert!(command.contains("--network none --detach"));
        assert!(!command.contains("--ip"));
        assert!(command.contains("--publish 80:8080 --publish 443:8443"));
        // jiji-proxy never execs into a sibling container, unlike kamal-proxy's
        // `--health-check-cmd`, so none of that access is needed here.
        assert!(!command.contains("docker.sock"));
        assert!(!command.contains("--privileged"));
    }

    #[test]
    fn docker_run_attaches_the_given_network_as_primary_at_creation() {
        let net = ProxyRunNetwork {
            bridge_name: "jiji-demo-9f8e7d6c",
            proxy_address: "10.0.2.9".parse().unwrap(),
        };
        let command = render_run_command(BridgeEngineKind::Docker, Some(&net), "v1-docker");
        assert!(command.contains("--network jiji-demo-9f8e7d6c --ip 10.0.2.9 --detach"));
        assert!(!command.contains("--network none"));
    }

    #[test]
    fn podman_run_needs_no_toolchain_mounts() {
        // Unlike kamal-proxy, which needed the host's whole Podman toolchain bind-mounted in for
        // its own `--health-check-cmd` exec, jiji-proxy is engine-agnostic at the container level:
        // it never touches the container runtime at all.
        let command = render_run_command(BridgeEngineKind::Podman, None, "v1-podman");
        assert!(!command.contains("--privileged"));
        assert!(!command.contains("/var/lib/containers"));
        assert!(!command.contains("/usr/local/bin/crun"));
    }

    #[test]
    fn run_command_mounts_certs_read_write_and_config_read_only() {
        let command = render_run_command(BridgeEngineKind::Docker, None, "v1-docker");
        assert!(command.contains("--volume /etc/jiji/certs:/etc/jiji/certs "));
        assert!(command.contains("--volume /etc/jiji/proxy:/etc/jiji/proxy:ro"));
    }

    #[test]
    fn daemon_config_enables_acme_with_no_contact_email_and_matches_internal_ports() {
        let config = render_daemon_config();
        assert!(config.contains("http_listen: \"0.0.0.0:8080\""));
        assert!(config.contains("https_listen: \"0.0.0.0:8443\""));
        assert!(config.contains("cert_dir: /etc/jiji/certs"));
        assert!(config.contains("directory_url:"));
        assert!(!config.contains("contact_email"));
    }

    #[test]
    fn attached_address_finds_the_named_network_and_ignores_others() {
        let json = r#"{"jiji-other-1a2b3c4d":{"IPAddress":"10.0.1.5"},"jiji-demo-9f8e7d6c":{"IPAddress":"10.0.2.9"}}"#;
        assert_eq!(
            attached_address(json, "jiji-demo-9f8e7d6c"),
            Some("10.0.2.9".parse().unwrap())
        );
        assert_eq!(attached_address(json, "jiji-missing"), None);
    }

    #[test]
    fn attached_address_handles_none_and_empty_address() {
        assert_eq!(attached_address("{}", "jiji-demo"), None);
        assert_eq!(
            attached_address(r#"{"jiji-demo":{"IPAddress":""}}"#, "jiji-demo"),
            None
        );
        assert_eq!(attached_address("null", "jiji-demo"), None);
        assert_eq!(attached_address("not json", "jiji-demo"), None);
    }

    #[test]
    fn nftables_dnats_both_public_ports_to_the_given_address_in_prerouting_only() {
        let rendered = render_nftables(
            "100.107.192.4".parse().unwrap(),
            "203.0.113.10".parse().unwrap(),
            true,
            &[],
        );
        assert!(rendered.starts_with(&format!("delete table ip {INGRESS_TABLE}\n")));
        assert_eq!(
            rendered
                .matches("ip daddr 203.0.113.10 tcp dport 80 dnat to 100.107.192.4:8080")
                .count(),
            1
        );
        assert_eq!(
            rendered
                .matches("ip daddr 203.0.113.10 tcp dport 443 dnat to 100.107.192.4:8443")
                .count(),
            1
        );
        assert!(rendered.contains("chain prerouting"));
        // Regression guard: a `chain output` DNAT here has no destination-address restriction and
        // silently hijacks every one of the host's own outbound port 80/443 connections anywhere
        // on the internet, not just requests to the host's own public IP (confirmed live).
        assert!(!rendered.contains("chain output"));
    }

    #[test]
    fn nftables_dnats_every_tcp_route_to_its_own_address_preserving_its_port() {
        // The host-level DNAT preserves a TCP route's port (address-only rewrite): the remap to
        // the shared internal relay port happens later, inside jiji-proxy's own container
        // namespace (`render_relay_netns_nftables`) -- see `render_nftables`'s own doc comment for
        // why (SO_ORIGINAL_DST only recovers a rewrite from the same namespace it happened in).
        let rendered = render_nftables(
            "100.107.192.4".parse().unwrap(),
            "203.0.113.10".parse().unwrap(),
            true,
            &[5432, 6379],
        );
        assert_eq!(
            rendered
                .matches("ip daddr 203.0.113.10 tcp dport 5432 dnat to 100.107.192.4:5432")
                .count(),
            1
        );
        assert_eq!(
            rendered
                .matches("ip daddr 203.0.113.10 tcp dport 6379 dnat to 100.107.192.4:6379")
                .count(),
            1
        );
        assert!(!rendered.contains("39100"));
        // The HTTP lines are always present when include_http is set.
        assert!(rendered.contains("tcp dport 80 dnat to 100.107.192.4:8080"));
        assert!(rendered.contains("tcp dport 443 dnat to 100.107.192.4:8443"));
    }

    #[test]
    fn nftables_omits_http_lines_when_include_http_is_false() {
        // Podman's own bridges don't disable IP masquerade, so its native `--publish 80/443`
        // already works there -- a second, independent DNAT rule for the same ports risks an
        // unpredictable interaction between two competing `prerouting` chains. A TCP route's
        // dynamic public port has no such native alternative on either engine, so it's always
        // included regardless.
        let rendered = render_nftables(
            "100.107.192.4".parse().unwrap(),
            "203.0.113.10".parse().unwrap(),
            false,
            &[5432],
        );
        assert!(!rendered.contains("dport 80 "));
        assert!(!rendered.contains("dport 443 "));
        assert!(rendered.contains("tcp dport 5432 dnat to 100.107.192.4:5432"));
    }

    #[test]
    fn relay_netns_nftables_maps_every_route_port_to_the_shared_internal_relay_port() {
        let rendered = render_relay_netns_nftables(&[5432, 6379]);
        assert!(rendered.starts_with(&format!("delete table ip {RELAY_NAT_TABLE}\n")));
        assert_eq!(rendered.matches("tcp dport 5432 dnat to :39100").count(), 1);
        assert_eq!(rendered.matches("tcp dport 6379 dnat to :39100").count(), 1);
        // No address restriction here: this table lives inside jiji-proxy's own netns, where every
        // locally-arriving packet on these ports is already this container's own traffic.
        assert!(!rendered.contains("ip daddr"));
    }

    #[test]
    fn relay_netns_nftables_still_clears_the_table_when_there_are_no_routes() {
        let rendered = render_relay_netns_nftables(&[]);
        assert!(rendered.starts_with(&format!("delete table ip {RELAY_NAT_TABLE}\n")));
        assert!(!rendered.contains("dnat to"));
    }

    #[test]
    fn relay_netns_apply_script_nsenters_into_the_given_pid_and_embeds_the_ruleset() {
        let script = render_relay_netns_apply_script(4242, &[5432]);
        assert!(
            script.contains("nsenter --net=/proc/4242/ns/net -- nft add table ip jiji_tcp_relay")
        );
        assert!(script.contains("nsenter --net=/proc/4242/ns/net -- nft -f - <<'JIJI_RELAY_NFT'"));
        assert!(script.contains("tcp dport 5432 dnat to :39100"));
        assert!(script.trim_end().ends_with("JIJI_RELAY_NFT"));
    }

    #[test]
    fn nftables_never_renders_a_forward_chain() {
        // render_nftables no longer attempts a forward-hook chain of its own -- confirmed live
        // that an independently registered nftables base chain's accept verdict does not override
        // a later chain's own verdict at the same hook, so this table never even tries; see
        // render_forward_accept_script for the fix that actually works.
        let rendered = render_nftables(
            "100.107.192.4".parse().unwrap(),
            "203.0.113.10".parse().unwrap(),
            false,
            &[5432],
        );
        assert!(!rendered.contains("chain forward"));
    }

    #[test]
    fn forward_accept_script_covers_http_ports_and_each_tcp_routes_own_port() {
        let script = render_forward_accept_script("100.107.192.4".parse().unwrap(), &[5432, 6379]);
        assert!(
            script.contains("ensure_rule FORWARD -d 100.107.192.4 -p tcp --dport 8080 -j ACCEPT")
        );
        assert!(
            script.contains("ensure_rule FORWARD -d 100.107.192.4 -p tcp --dport 8443 -j ACCEPT")
        );
        // The host-side DNAT preserves each route's own public port (address-only rewrite), so the
        // packet reaching this hook still carries that port, not the shared internal relay port --
        // each configured route's port must be opened individually.
        assert!(
            script.contains("ensure_rule FORWARD -d 100.107.192.4 -p tcp --dport 5432 -j ACCEPT")
        );
        assert!(
            script.contains("ensure_rule FORWARD -d 100.107.192.4 -p tcp --dport 6379 -j ACCEPT")
        );
        assert!(!script.contains("--dport 39100"));
        assert!(script.contains("iptables -C"));
        assert!(script.contains("DOCKER-USER"));
    }

    #[test]
    fn forward_accept_script_always_covers_http_ports_regardless_of_engine() {
        // jiji-proxy always listens on both internal HTTP ports regardless of route
        // configuration or which engine's DNAT delivered the packet here (confirmed live:
        // Podman's own *native* --publish DNAT for 80/443 hits this exact hook too).
        let script = render_forward_accept_script("100.107.192.4".parse().unwrap(), &[]);
        assert!(script.contains("--dport 8080"));
        assert!(script.contains("--dport 8443"));
    }

    #[test]
    fn nftables_never_matches_without_a_destination_address_restriction() {
        // Regression guard: `prerouting` fires for every packet arriving on any interface before
        // the routing decision, including bridge-originated traffic merely transiting this host on
        // its way to a *remote* peer over WireGuard. Without an `ip daddr` restriction, ordinary
        // cross-host mesh traffic on port 80/443 gets silently hijacked back to this host's own
        // proxy instead of ever reaching its real destination (confirmed live, kamal-proxy era).
        let rendered = render_nftables(
            "100.107.192.4".parse().unwrap(),
            "203.0.113.10".parse().unwrap(),
            true,
            &[5432],
        );
        for line in rendered.lines().filter(|line| line.contains("dnat to")) {
            assert!(
                line.contains("ip daddr"),
                "DNAT rule missing a destination-address restriction: {line}"
            );
        }
    }

    #[test]
    fn surviving_address_ignores_non_jiji_networks() {
        assert_eq!(
            surviving_proxy_address(
                "bridge 172.17.0.2\njiji-other 100.107.192.4\njiji-third 100.107.200.4\n"
            ),
            Some("100.107.192.4".parse().unwrap())
        );
        assert_eq!(surviving_proxy_address("bridge 172.17.0.2\n"), None);
    }
}
