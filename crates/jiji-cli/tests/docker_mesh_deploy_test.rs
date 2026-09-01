//! Docker-in-Docker integration suite, multi-host: proves the core "distributed" claim in
//! AGENTS.md's "Private Networking" section actually holds against a real second host, not just
//! `vm1` alone. `jiji server setup` against both `vm1` and `vm2` (see `test/docker/compose.yml`'s
//! `mesh` network) must bring up a real WireGuard peer connection between them; `jiji deploy` of a
//! two-replica service must replicate both replicas' catalog records into each host's own
//! `.jiji`-zone DNS resolver; and each host must be able to reach the other's replica address
//! directly, over the WireGuard tunnel, not just know about it.
//!
//! Requires `JIJI_DOCKER_TESTS=1` and the compose stack already up (`mise test-docker` runs
//! both); otherwise this test skips itself (`docker_support::skip_unless_enabled`).

mod docker_support;

use std::time::Duration;

fn wireguard_handshake_completed(service: &str, iface: &str) -> bool {
    let output =
        docker_support::exec_service_stdout(service, &["wg", "show", iface, "latest-handshakes"]);
    output.lines().any(|line| {
        line.split_whitespace()
            .nth(1)
            .is_some_and(|timestamp| timestamp != "0")
    })
}

#[test]
fn wireguard_mesh_connects_two_hosts_and_replicates_catalog_across_them() {
    if docker_support::skip_unless_enabled() {
        return;
    }

    docker_support::wait_for_service_ready("vm1", Duration::from_secs(60));
    docker_support::wait_for_service_ready("vm2", Duration::from_secs(60));

    let dir = tempfile::tempdir().expect("create tempdir");
    let project = "jijidockermesh";
    let config_path = docker_support::write_config_with_two_host_service(dir.path(), project);

    let setup = docker_support::run_jiji(&config_path, &["server", "setup", "-y"]);
    assert!(
        setup.status.success(),
        "jiji server setup failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&setup.stdout),
        String::from_utf8_lossy(&setup.stderr),
    );

    let iface = jiji_network::wireguard_interface_name(project);
    for service in ["vm1", "vm2"] {
        assert!(
            wireguard_handshake_completed(service, &iface),
            "expected a completed WireGuard handshake on {service}, got: {}",
            docker_support::exec_service_stdout(
                service,
                &["wg", "show", &iface, "latest-handshakes"]
            )
        );
    }

    let deploy = docker_support::run_jiji(&config_path, &["deploy", "-y"]);
    assert!(
        deploy.status.success(),
        "jiji deploy failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&deploy.stdout),
        String::from_utf8_lossy(&deploy.stderr),
    );

    let vm1_dns = docker_support::dns_address_for(&config_path, "vm1");
    let aggregate_name = format!("{project}-web.jiji");
    // Cross-host catalog/DNS anti-entropy converges asynchronously after `deploy` returns, so a
    // single query right after success can race vm2's replica record not having propagated to
    // vm1's own local catalog yet (confirmed live, not just a theoretical race).
    let replica_addresses = docker_support::wait_for_dns_replica_count(
        "vm1",
        vm1_dns,
        &aggregate_name,
        2,
        Duration::from_secs(30),
    );

    // Real cross-host L3 reachability, not just DNS knowledge: vm1 must be able to curl every
    // replica address directly, including vm2's, over the WireGuard tunnel. `curl` comes from
    // `jiji server setup`'s own Podman install prerequisites, not the base vm image.
    for address in &replica_addresses {
        let curl = docker_support::exec_service(
            "vm1",
            &[
                "curl",
                "-fsS",
                "--max-time",
                "5",
                &format!("http://{address}/"),
            ],
        );
        assert!(
            curl.status.success(),
            "vm1 could not reach replica address {address} directly over the mesh: {}",
            String::from_utf8_lossy(&curl.stderr)
        );
    }

    let remove = docker_support::run_jiji(&config_path, &["service", "remove", "-y", "-S", "web"]);
    assert!(
        remove.status.success(),
        "jiji service remove failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&remove.stdout),
        String::from_utf8_lossy(&remove.stderr),
    );

    let teardown = docker_support::run_jiji(&config_path, &["server", "teardown", "-y"]);
    assert!(
        teardown.status.success(),
        "jiji server teardown failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&teardown.stdout),
        String::from_utf8_lossy(&teardown.stderr),
    );
}
