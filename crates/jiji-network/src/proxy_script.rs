//! Pure rendering/parsing for the shared, host-global kamal-proxy container and its Docker-only
//! nftables ingress DNAT workaround. Shared by `jiji-cli` (`proxy.rs`/`proxy_ingress.rs`, driven
//! over SSH) and `jiji-agent` (its own native reconciliation) so neither has to duplicate -- and
//! risk drifting from -- the other's understanding of kamal-proxy's run command, image
//! fingerprint, or DNAT rules.

use std::net::Ipv4Addr;

use crate::BridgeEngineKind;

pub const CONTAINER_NAME: &str = "kamal-proxy";
pub const IMAGE: &str = "ghcr.io/acidtib/kamal-proxy:jiji";
pub const CONFIG_VOLUME: &str = "kamal-proxy-config";
pub const CERTS_DIR: &str = "/etc/jiji/certs";
pub const INTERNAL_HTTP_PORT: u16 = 8080;
pub const INTERNAL_HTTPS_PORT: u16 = 8443;
pub const INGRESS_TABLE: &str = "jiji_proxy_ingress";

/// The one project bridge kamal-proxy should be attached to as its *primary* network at creation
/// time (every other project's bridge is attached afterward via `network connect`, additive).
pub struct ProxyRunNetwork<'a> {
    pub bridge_name: &'a str,
    pub proxy_address: Ipv4Addr,
}

/// Identity used purely to decide "does the running container need replacing" (image/engine
/// drift) -- deliberately excludes any project's network address, since kamal-proxy can be
/// attached to several projects' bridges at once and none of them singularly identifies "the"
/// container's configuration anymore.
pub fn config_fingerprint(engine: BridgeEngineKind) -> String {
    format!("v6-{engine}")
}

pub fn render_run_command(
    engine: BridgeEngineKind,
    network: Option<&ProxyRunNetwork<'_>>,
    fingerprint: &str,
) -> String {
    let runtime = match engine {
        BridgeEngineKind::Docker => {
            " --volume /var/run/docker.sock:/var/run/docker.sock".to_string()
        }
        // Confirmed live: on a host whose Podman was installed via `engine::ensure_engine`'s
        // pinned static-binary path (`mgoltzsche/podman-static`, the default on Debian/Ubuntu,
        // see "Container Engine Provisioning" in CLAUDE.md), the entire toolchain --
        // `podman`/`crun`/`runc`/`fuse-overlayfs`/`fusermount3`/`pasta` -- lives
        // under `/usr/local/bin` (not the distro-standard `/usr/bin` this container's mount list
        // originally assumed), the runtime is pinned to the exact path `/usr/local/bin/crun` by
        // `/etc/containers/containers.conf.d/99-jiji-static.conf`, and `conmon`/`netavark`/
        // `aardvark-dns` live under `/usr/local/lib/podman`. Without every one of these,
        // kamal-proxy's own bundled `podman` client can see a sibling container's stored metadata
        // (via the shared `/var/lib/containers` storage) recording it was created with runtime
        // `/usr/local/bin/crun`, and storage driver `overlay` (needing `fuse-overlayfs`), but
        // can't find those binaries in its own filesystem view -- confirmed live, missing `crun`
        // failed every `--health-check-cmd` exec outright with "OCI Runtime .../crun ... is not
        // available", and missing `fuse-overlayfs` alone (crun/podman present, this file not yet
        // mounted) failed even a plain `podman ps -q` with "configure storage: overlay: can't
        // stat program /usr/local/bin/fuse-overlayfs". `/etc/containers` is mounted too so this
        // container's own `podman` resolves the exact same runtime/storage configuration as the
        // host.
        //
        // `/usr/local/bin` itself is deliberately NOT mounted as a whole directory: kamal-proxy's
        // own image places its `kamal-proxy` binary at that exact path (confirmed live: mounting
        // the host's `/usr/local/bin` over it shadows the image's own binary entirely, and the
        // container fails to start with "executable file `kamal-proxy` not found in $PATH"). Every
        // static-install helper binary is instead mounted individually by file, adding each into
        // the existing directory without replacing anything else already there.
        BridgeEngineKind::Podman => concat!(
            " --privileged --user root --pid=host --cgroupns=host",
            " --volume /run:/run",
            " --volume /usr/bin:/usr/bin:ro",
            " --volume /usr/lib:/usr/lib:ro",
            " --volume /usr/local/bin/podman:/usr/local/bin/podman:ro",
            " --volume /usr/local/bin/crun:/usr/local/bin/crun:ro",
            " --volume /usr/local/bin/runc:/usr/local/bin/runc:ro",
            " --volume /usr/local/bin/fuse-overlayfs:/usr/local/bin/fuse-overlayfs:ro",
            " --volume /usr/local/bin/fusermount3:/usr/local/bin/fusermount3:ro",
            // `pasta.avx2` (an AVX2-optimized build variant of `pasta`, AVX2 being an x86-only
            // SIMD extension) is deliberately excluded: confirmed live that Podman hard-fails a
            // bind mount whose source file doesn't exist ("statfs ... no such file or
            // directory"), so mounting an amd64-only optional variant unconditionally would break
            // container startup entirely on an arm64 host's static bundle, which never has it.
            // Plain `pasta` (mounted above) is the baseline, arch-general binary and is what
            // Podman actually needs for rootless networking regardless of AVX2 availability.
            " --volume /usr/local/bin/pasta:/usr/local/bin/pasta:ro",
            " --volume /usr/local/lib:/usr/local/lib:ro",
            " --volume /lib:/lib:ro",
            " --volume /lib64:/lib64:ro",
            " --volume /etc/containers:/etc/containers:ro",
            " --volume /var/lib/containers:/var/lib/containers"
        )
        .to_string(),
    };

    // `--network none` is a Docker/Podman-exclusive private mode: a container created with it can
    // never have a real network `connect`ed afterward (confirmed live). So the network that
    // triggered this (re)creation must be attached right here, as the primary network; every other
    // project's bridge is added afterward via `network connect`, which works fine once there's at
    // least one real network already.
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
         --volume {CONFIG_VOLUME}:/home/kamal-proxy/.config/kamal-proxy \
         --volume {CERTS_DIR}:/jiji-certs:ro{runtime} \
         --publish 80:{INTERNAL_HTTP_PORT} --publish 443:{INTERNAL_HTTPS_PORT} \
         {IMAGE} kamal-proxy run --http-port {INTERNAL_HTTP_PORT} \
         --https-port {INTERNAL_HTTPS_PORT}"
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

/// DNATs both public ports to a single currently-attached kamal-proxy address. Never needs to
/// represent every co-resident project's routes: kamal-proxy is one process listening on every
/// attached interface, so any currently-attached address reaches the same process, which does its
/// own per-request routing by Host header -- not by which address the DNAT rule happened to name.
///
/// Both rules are restricted to `ip daddr {public_host}` -- this host's own public IP -- for the
/// same reason a `chain output` DNAT (present in an earlier version of this ruleset, matching
/// locally-*generated* packets by destination port alone) had to be removed entirely: with no
/// destination-address restriction, `prerouting` matches on destination *port* alone too, and
/// `prerouting` fires for every packet arriving on any interface before the routing decision --
/// including bridge-originated traffic merely passing through this host on its way to a *remote*
/// peer over WireGuard. Confirmed live: a container calling a *different* host's replica on port
/// 80 (ordinary cross-host mesh traffic, e.g. kamal-proxy health-checking a remote target) got
/// silently hijacked back to this host's own kamal-proxy instead of ever reaching the real
/// destination, because the rule matched on `tcp dport 80` with no regard for where the packet was
/// actually headed. Restricting to `ip daddr {public_host}` scopes both rules to their only
/// intended case: traffic genuinely arriving from the WAN, addressed to this host's own public IP.
pub fn render_nftables(address: Ipv4Addr, public_host: Ipv4Addr) -> String {
    format!(
        "delete table ip {INGRESS_TABLE}\n\
         table ip {INGRESS_TABLE} {{\n\
         \tchain prerouting {{\n\
         \t\ttype nat hook prerouting priority dstnat - 5; policy accept;\n\
         \t\tip daddr {public_host} tcp dport 80 dnat to {address}:{INTERNAL_HTTP_PORT}\n\
         \t\tip daddr {public_host} tcp dport 443 dnat to {address}:{INTERNAL_HTTPS_PORT}\n\
         \t}}\n\
         }}\n"
    )
}

/// Finds a still-attached jiji bridge address from `{{range ...}}{{printf "%s %s\n" $name
/// $network.IPAddress}}{{end}}`-shaped inspect output, for recovering the ingress rule after
/// kamal-proxy survives but its ingress state was lost.
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
    fn docker_run_omits_network_when_none_given_and_never_sets_dns() {
        let command = render_run_command(BridgeEngineKind::Docker, None, "v3-docker");
        assert!(command.contains("ghcr.io/acidtib/kamal-proxy:jiji"));
        assert!(command.contains("--network none --detach"));
        assert!(!command.contains("--ip"));
        assert!(!command.contains("--dns"));
        assert!(command.contains("--publish 80:8080 --publish 443:8443"));
        assert!(command.contains("/var/run/docker.sock:/var/run/docker.sock"));
    }

    #[test]
    fn docker_run_attaches_the_given_network_as_primary_at_creation() {
        let net = ProxyRunNetwork {
            bridge_name: "jiji-demo-9f8e7d6c",
            proxy_address: "10.0.2.9".parse().unwrap(),
        };
        let command = render_run_command(BridgeEngineKind::Docker, Some(&net), "v3-docker");
        assert!(command.contains("--network jiji-demo-9f8e7d6c --ip 10.0.2.9 --detach"));
        assert!(!command.contains("--network none"));
    }

    #[test]
    fn podman_run_has_command_health_check_access() {
        let command = render_run_command(BridgeEngineKind::Podman, None, "v3-podman");
        assert!(command.contains("--privileged --user root --pid=host --cgroupns=host"));
        assert!(command.contains("/var/lib/containers:/var/lib/containers"));
        assert!(!command.contains("docker.sock"));
    }

    #[test]
    fn podman_run_mounts_the_static_podman_install_location_and_its_config() {
        // Regression guard: a host whose Podman came from `engine::ensure_engine`'s pinned
        // static-binary install (the default on Debian/Ubuntu) has its whole toolchain under
        // `/usr/local/bin`, not the distro-standard `/usr/bin` -- confirmed live, kamal-proxy's
        // own `--health-check-cmd` exec failed with "OCI Runtime /usr/local/bin/crun ... is not
        // available" (missing `crun`) and then "configure storage: overlay: can't stat program
        // /usr/local/bin/fuse-overlayfs" (missing `fuse-overlayfs`) without these mounted in.
        let command = render_run_command(BridgeEngineKind::Podman, None, "v6-podman");
        assert!(command.contains("--volume /usr/local/bin/podman:/usr/local/bin/podman:ro"));
        assert!(command.contains("--volume /usr/local/bin/crun:/usr/local/bin/crun:ro"));
        assert!(command.contains("--volume /usr/local/bin/runc:/usr/local/bin/runc:ro"));
        assert!(command
            .contains("--volume /usr/local/bin/fuse-overlayfs:/usr/local/bin/fuse-overlayfs:ro"));
        assert!(
            command.contains("--volume /usr/local/bin/fusermount3:/usr/local/bin/fusermount3:ro")
        );
        assert!(command.contains("--volume /usr/local/bin/pasta:/usr/local/bin/pasta:ro"));
        assert!(command.contains("--volume /usr/local/lib:/usr/local/lib:ro"));
        assert!(command.contains("--volume /etc/containers:/etc/containers:ro"));
    }

    #[test]
    fn podman_run_never_mounts_the_whole_usr_local_bin_directory_or_the_amd64_only_pasta_variant() {
        // Regression guard for the fix above's own first (broken) attempt: mounting the host's
        // whole `/usr/local/bin` directory shadows the image's own `/usr/local/bin/kamal-proxy`
        // binary entirely -- confirmed live, the container then fails to start with "executable
        // file `kamal-proxy` not found in $PATH". Only individual files may be mounted there.
        // `pasta.avx2` is excluded for a related reason: it's an AVX2 (x86-only) build variant,
        // and Podman hard-fails a bind mount whose source file doesn't exist at all -- mounting
        // it unconditionally would break every arm64 host outright.
        let command = render_run_command(BridgeEngineKind::Podman, None, "v6-podman");
        assert!(!command.contains("--volume /usr/local/bin:/usr/local/bin:ro"));
        assert!(!command.contains("pasta.avx2"));
    }

    #[test]
    fn config_fingerprint_was_bumped_for_the_static_podman_mount_fix() {
        // The fingerprint is what decides whether an already-running kamal-proxy container gets
        // replaced on the next non-forced `ensure_proxy` call -- since this mount fix changes real
        // container behavior (not just cosmetic rendering), it must bump the version so a host
        // running a pre-fix container upgrades automatically instead of silently keeping its old,
        // broken mount list forever.
        assert_eq!(config_fingerprint(BridgeEngineKind::Podman), "v6-podman");
        assert_eq!(config_fingerprint(BridgeEngineKind::Docker), "v6-docker");
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
    fn nftables_never_matches_without_a_destination_address_restriction() {
        // Regression guard: `prerouting` fires for every packet arriving on any interface before
        // the routing decision, including bridge-originated traffic merely transiting this host on
        // its way to a *remote* peer over WireGuard. Without an `ip daddr` restriction, ordinary
        // cross-host mesh traffic on port 80/443 gets silently hijacked back to this host's own
        // kamal-proxy instead of ever reaching its real destination (confirmed live).
        let rendered = render_nftables(
            "100.107.192.4".parse().unwrap(),
            "203.0.113.10".parse().unwrap(),
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
