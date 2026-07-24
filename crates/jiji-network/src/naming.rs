//! Single source of truth for every project-derived name jiji installs on a host: the WireGuard
//! interface, the container-engine bridge (logical name and kernel device name are two different
//! things with different constraints, see below), systemd unit names, and the WireGuard port.
//!
//! Each project on a host gets its own instance of all of these, computed purely from
//! `config.project` -- no coordination with, or knowledge of, any other project that might share
//! the same host. Interface/device names are salted, short hashes rather than sanitized project
//! text: Linux interface names are capped at 15 characters (`IFNAMSIZ`), and a project name has no
//! guaranteed length or character set (see `jiji-config`'s validation, which only checks
//! presence).

use sha2::{Digest, Sha256};

pub(crate) fn stable_hash(value: &[u8]) -> u64 {
    let digest = Sha256::digest(value);
    u64::from_be_bytes(
        digest[..8]
            .try_into()
            .expect("SHA-256 has at least 8 bytes"),
    )
}

/// Independent per-purpose hashes of the same project name, so the WireGuard interface, bridge
/// device, and port don't visibly share digits with each other (they're unrelated resources).
fn salted_hash(salt: &str, project: &str) -> u64 {
    stable_hash(format!("{salt}:{project}").as_bytes())
}

/// WireGuard interface name, e.g. `jiji1a2b3c4d` (12 chars, well under the 15-char `IFNAMSIZ`
/// limit). Also the `wg-quick@` systemd instance name, which needs no separate scoping since it's
/// already systemd's own per-instance template mechanism.
pub fn wireguard_interface_name(project: &str) -> String {
    format!("jiji{:08x}", salted_hash("wg", project) as u32)
}

/// Kernel bridge device name, e.g. `jijib1a2b3c4` (12 chars). Deliberately distinct from
/// [`bridge_network_name`]: the Docker/Podman *logical* network name has no length limit, but
/// `--opt com.docker.network.bridge.name=`/`--interface-name` sets the literal kernel interface
/// name, which is subject to the same 15-char limit as the WireGuard interface.
pub fn bridge_interface_name(project: &str) -> String {
    format!(
        "jijib{:07x}",
        salted_hash("br", project) as u32 & 0x0fff_ffff
    )
}

/// Docker/Podman logical network name, e.g. `jiji-my-app-1a2b3c4d`. Unconstrained length, so this
/// one stays human-readable; the trailing 8-hex-char hash is load-bearing, not decorative -- it
/// guarantees two project names that sanitize to the same slug (`"My App"` vs `"my_app"`) don't
/// collide.
pub fn bridge_network_name(project: &str) -> String {
    format!("jiji-{}", project_slug(project))
}

/// Shared slug used to build every project-scoped systemd unit name (`jiji-dns-{slug}.service`,
/// `jiji-service-nat-{slug}.service`, `jiji-network-restore-{slug}.service`, the podman-restart
/// drop-in filename).
pub fn systemd_unit_slug(project: &str) -> String {
    project_slug(project)
}

fn project_slug(project: &str) -> String {
    let mut slug = String::new();
    for ch in project.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() {
            slug.push(lower);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug.truncate(20);
    while slug.ends_with('-') {
        slug.pop();
    }

    let hash = format!("{:08x}", stable_hash(project.as_bytes()) as u32);
    if slug.is_empty() {
        hash
    } else {
        format!("{slug}-{hash}")
    }
}

/// Deterministic WireGuard UDP port in `51820..=55819`, one per project (not per server, matching
/// the single-port-per-fleet semantics every server in a project already shared before this
/// module existed).
pub fn wireguard_port(project: &str) -> u16 {
    51820 + (salted_hash("port", project) % 4000) as u16
}

/// nftables table name for the VIP/NAT chain, e.g. `jiji_service_nat_myapp_1a2b3c4d`. Unlike
/// [`bridge_network_name`]/[`systemd_unit_slug`], nftables identifiers don't accept `-` unless
/// quoted, so this reuses the same slug with hyphens swapped for underscores rather than
/// introducing a separate slug scheme.
pub fn service_nat_table_name(project: &str) -> String {
    format!(
        "jiji_service_nat_{}",
        systemd_unit_slug(project).replace('-', "_")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adversarial_inputs() -> Vec<&'static str> {
        vec![
            "demo",
            "",
            "My App",
            "my_app",
            "a-very-long-project-name-that-goes-on-and-on-and-on",
            "unicode-\u{1F600}-project",
            "has/slashes/and spaces",
            "UPPERCASE",
            "---",
            "123-numeric-start",
        ]
    }

    #[test]
    fn wireguard_interface_name_always_fits_ifnamsiz_and_is_hex_only() {
        for project in adversarial_inputs() {
            let name = wireguard_interface_name(project);
            assert!(name.len() <= 15, "{project:?} -> {name:?}");
            assert!(name.starts_with("jiji"));
            assert!(name[4..].chars().all(|c| c.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn bridge_interface_name_always_fits_ifnamsiz_and_is_hex_only() {
        for project in adversarial_inputs() {
            let name = bridge_interface_name(project);
            assert!(name.len() <= 15, "{project:?} -> {name:?}");
            assert!(name.starts_with("jijib"));
            assert!(name[5..].chars().all(|c| c.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn bridge_network_name_and_systemd_unit_slug_use_only_safe_characters() {
        for project in adversarial_inputs() {
            let network_name = bridge_network_name(project);
            assert!(network_name.starts_with("jiji-"));
            assert!(network_name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'));
            assert!(!network_name.contains("--"));

            let slug = systemd_unit_slug(project);
            assert!(slug
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'));
            assert!(!slug.starts_with('-') && !slug.ends_with('-'));
        }
    }

    #[test]
    fn wireguard_port_is_always_in_range() {
        for project in adversarial_inputs() {
            let port = wireguard_port(project);
            assert!((51820..=55819).contains(&port), "{project:?} -> {port}");
        }
    }

    #[test]
    fn all_derivations_are_deterministic() {
        for project in adversarial_inputs() {
            assert_eq!(
                wireguard_interface_name(project),
                wireguard_interface_name(project)
            );
            assert_eq!(
                bridge_interface_name(project),
                bridge_interface_name(project)
            );
            assert_eq!(bridge_network_name(project), bridge_network_name(project));
            assert_eq!(systemd_unit_slug(project), systemd_unit_slug(project));
            assert_eq!(wireguard_port(project), wireguard_port(project));
        }
    }

    #[test]
    fn a_project_rename_changes_every_derived_name() {
        let a = "project-a";
        let b = "project-b";
        assert_ne!(wireguard_interface_name(a), wireguard_interface_name(b));
        assert_ne!(bridge_interface_name(a), bridge_interface_name(b));
        assert_ne!(bridge_network_name(a), bridge_network_name(b));
        assert_ne!(systemd_unit_slug(a), systemd_unit_slug(b));
        // wireguard_port has a small (4000-wide) range, so two arbitrary names could coincide by
        // chance; assert the hash function is actually being exercised per-project instead of a
        // fixed constant, using a wider spread that would trivially fail if it weren't.
        let ports: std::collections::BTreeSet<u16> = (0..25)
            .map(|index| wireguard_port(&format!("project-{index}")))
            .collect();
        assert!(ports.len() > 1, "port should vary across projects");
    }
}
