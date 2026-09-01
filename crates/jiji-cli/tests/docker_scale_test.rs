//! Docker-in-Docker integration suite, `scale`: proves per-host multi-replica placement (`scale:
//! N` on one server) actually runs N distinct containers with N distinct leased addresses, both
//! reachable through jiji-proxy, and that `jiji service scale` can shrink that back down on a live
//! service, removing the retired replica's container outright rather than just stopping it.
//!
//! `docker_load_balancer_test.rs` already proves jiji-proxy alternates across replicas on two
//! different hosts; this proves the same for two replicas on the *same* host, which is what
//! `scale:` actually means (AGENTS.md: "the instance count on *each* one, not a total spread
//! across a pool").
//!
//! Requires `JIJI_DOCKER_TESTS=1` and the compose stack already up (`mise test-docker` runs
//! both); otherwise this test skips itself (`docker_support::skip_unless_enabled`).

mod docker_support;

use std::time::Duration;

#[test]
fn scale_places_multiple_replicas_on_one_host_and_service_scale_shrinks_them() {
    if docker_support::skip_unless_enabled() {
        return;
    }

    docker_support::wait_for_vm1_ready(Duration::from_secs(60));

    let dir = tempfile::tempdir().expect("create tempdir");
    let project = "jijidockerscale";
    let config_path = docker_support::write_config_with_scalable_service(dir.path(), project);

    let setup = docker_support::run_jiji(&config_path, &["server", "setup", "-y"]);
    assert!(
        setup.status.success(),
        "jiji server setup failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&setup.stdout),
        String::from_utf8_lossy(&setup.stderr),
    );

    let deploy = docker_support::run_jiji(&config_path, &["deploy", "-y"]);
    assert!(
        deploy.status.success(),
        "jiji deploy failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&deploy.stdout),
        String::from_utf8_lossy(&deploy.stderr),
    );

    docker_support::wait_for_http_ok(
        docker_support::SCALE_TEST_HOST,
        "/",
        Duration::from_secs(30),
    );

    let containers_at_scale_2 = docker_support::all_container_ids_for_service("web");
    assert_eq!(
        containers_at_scale_2.len(),
        2,
        "expected exactly two containers for service 'web' at scale: 2, got: {containers_at_scale_2:?}"
    );

    let bodies_at_scale_2 = docker_support::distinct_response_bodies(
        docker_support::SCALE_TEST_HOST,
        "/",
        2,
        Duration::from_secs(30),
    );
    assert!(
        bodies_at_scale_2.len() >= 2,
        "expected jiji-proxy to alternate between both same-host replicas' distinct hostnames within 30s, only saw: {bodies_at_scale_2:?}"
    );

    let scale_down =
        docker_support::run_jiji(&config_path, &["service", "scale", "1", "-S", "web", "-y"]);
    assert!(
        scale_down.status.success(),
        "jiji service scale 1 failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&scale_down.stdout),
        String::from_utf8_lossy(&scale_down.stderr),
    );

    let containers_at_scale_1 = docker_support::all_container_ids_for_service("web");
    assert_eq!(
        containers_at_scale_1.len(),
        1,
        "expected the retired replica's container to be removed outright after scaling down to 1, got: {containers_at_scale_1:?}"
    );

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
