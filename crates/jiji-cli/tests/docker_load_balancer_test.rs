//! Docker-in-Docker integration suite, proxy load balancing: `docker_mesh_deploy_test.rs` already
//! proves vm1 and vm2 are both individually reachable over the WireGuard mesh and known to each
//! other's catalog/DNS -- it deliberately does not prove jiji-proxy itself alternates between
//! them. This does: two replicas, one per host, each serving its own container's hostname as its
//! whole response body, so repeated requests through jiji-proxy can prove real backend
//! alternation by the sheer fact that more than one distinct body comes back.
//!
//! Requires `JIJI_DOCKER_TESTS=1` and the compose stack already up (`mise test-docker` runs
//! both); otherwise this test skips itself (`docker_support::skip_unless_enabled`).

mod docker_support;

use std::time::Duration;

#[test]
fn proxy_alternates_between_replicas_on_different_hosts() {
    if docker_support::skip_unless_enabled() {
        return;
    }

    docker_support::wait_for_service_ready("vm1", Duration::from_secs(60));
    docker_support::wait_for_service_ready("vm2", Duration::from_secs(60));

    let dir = tempfile::tempdir().expect("create tempdir");
    let project = "jijidockerlb";
    let config_path = docker_support::write_config_with_load_balanced_service(dir.path(), project);

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
        docker_support::LOAD_BALANCER_TEST_HOST,
        "/",
        Duration::from_secs(30),
    );

    let bodies =
        docker_support::distinct_response_bodies(docker_support::LOAD_BALANCER_TEST_HOST, "/", 40);
    assert!(
        bodies.len() >= 2,
        "expected jiji-proxy to alternate between both replicas' distinct hostnames across 40 requests, only saw: {bodies:?}"
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
