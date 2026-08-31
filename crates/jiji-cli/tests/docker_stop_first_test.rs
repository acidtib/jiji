//! Docker-in-Docker integration suite, `stop_first`: proves the "Direct host-port bindings cannot
//! coexist during replacement" invariant AGENTS.md documents under "Health-Gated Deployment
//! Strategy", and that `service.stop_first: true` is what actually resolves it.
//!
//! A service with a direct `ports:` host binding can never have two containers running at once:
//! podman refuses to bind an already-claimed host port. Without `stop_first`, a redeploy leases
//! and starts the *candidate* first, health-gated, before ever touching the previous container --
//! the candidate's own `podman run` fails outright (port already bound), and the previous
//! container is left completely untouched (the same rolling invariant
//! `docker_rolling_deploy_test.rs` proves for a failed health check, here for a failed container
//! start instead). With `stop_first: true`, the previous container is stopped first, freeing the
//! port before the candidate ever tries to bind it, so the same redeploy succeeds and a genuinely
//! new container replaces the old one.
//!
//! Requires `JIJI_DOCKER_TESTS=1` and the compose stack already up (`mise test-docker` runs
//! both); otherwise this test skips itself (`docker_support::skip_unless_enabled`).

mod docker_support;

use std::time::Duration;

#[test]
fn direct_port_binding_redeploy_needs_stop_first_to_succeed() {
    if docker_support::skip_unless_enabled() {
        return;
    }

    docker_support::wait_for_vm1_ready(Duration::from_secs(60));

    let dir = tempfile::tempdir().expect("create tempdir");
    let project = "jijidockerstopfirst";

    let config_no_stop_first =
        docker_support::write_config_with_direct_port_service(dir.path(), project, false);
    let setup = docker_support::run_jiji(&config_no_stop_first, &["server", "setup", "-y"]);
    assert!(
        setup.status.success(),
        "jiji server setup failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&setup.stdout),
        String::from_utf8_lossy(&setup.stderr),
    );

    let first_deploy = docker_support::run_jiji(&config_no_stop_first, &["deploy", "-y"]);
    assert!(
        first_deploy.status.success(),
        "first jiji deploy failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&first_deploy.stdout),
        String::from_utf8_lossy(&first_deploy.stderr),
    );
    let running_before = docker_support::all_container_ids_for_service("web");
    assert_eq!(
        running_before.len(),
        1,
        "expected exactly one container for service 'web' after the first deploy, got: {running_before:?}"
    );

    // Same config, no stop_first: the candidate tries to bind the same host port the still-active
    // previous container already holds, and must fail before the previous container is ever
    // touched.
    let second_deploy = docker_support::run_jiji(&config_no_stop_first, &["deploy", "-y"]);
    assert!(
        !second_deploy.status.success(),
        "expected the second deploy (no stop_first, port already bound) to fail, but it succeeded:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&second_deploy.stdout),
        String::from_utf8_lossy(&second_deploy.stderr),
    );
    let running_after_failure = docker_support::all_container_ids_for_service("web");
    assert_eq!(
        running_after_failure, running_before,
        "expected the exact same container to still be running after the failed deploy, previous: {running_before:?}, after: {running_after_failure:?}"
    );

    // Same service, stop_first: true: the previous container is stopped first, freeing the port
    // before the candidate ever tries to bind it, so this redeploy must succeed.
    let config_stop_first =
        docker_support::write_config_with_direct_port_service(dir.path(), project, true);
    let third_deploy = docker_support::run_jiji(&config_stop_first, &["deploy", "-y"]);
    assert!(
        third_deploy.status.success(),
        "third jiji deploy (stop_first: true) failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&third_deploy.stdout),
        String::from_utf8_lossy(&third_deploy.stderr),
    );
    let running_after_stop_first = docker_support::all_container_ids_for_service("web");
    assert_eq!(
        running_after_stop_first.len(),
        1,
        "expected exactly one container for service 'web' after the stop_first redeploy, got: {running_after_stop_first:?}"
    );
    assert_ne!(
        running_after_stop_first, running_before,
        "expected stop_first to actually replace the container, not reuse the old one"
    );

    let remove = docker_support::run_jiji(
        &config_stop_first,
        &["service", "remove", "-y", "-S", "web"],
    );
    assert!(
        remove.status.success(),
        "jiji service remove failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&remove.stdout),
        String::from_utf8_lossy(&remove.stderr),
    );

    let teardown = docker_support::run_jiji(&config_stop_first, &["server", "teardown", "-y"]);
    assert!(
        teardown.status.success(),
        "jiji server teardown failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&teardown.stdout),
        String::from_utf8_lossy(&teardown.stderr),
    );
}
