//! Docker-in-Docker integration suite, rolling-deploy invariant: proves the one
//! invariant AGENTS.md calls out as core and non-negotiable in "Health-Gated Deployment
//! Strategy" -- if a candidate never becomes healthy, the previously active container is left
//! completely untouched and keeps serving traffic, and only the candidate gets torn down.
//!
//! Rather than literally racing a container kill against the health-check window (flaky by
//! construction), this deploys once successfully, then redeploys the same service with a
//! healthcheck path that nginx will always 404 on and a short `deploy_timeout` -- a deterministic
//! way to trigger the exact same failure path (`deploy_transaction.rs`'s health check failure
//! branch, `release_candidate`) without any timing race.
//!
//! Requires `JIJI_DOCKER_TESTS=1` and the compose stack already up (`mise test-docker` runs
//! both); otherwise this test skips itself (`docker_support::skip_unless_enabled`).

mod docker_support;

use std::time::Duration;

#[test]
fn failed_candidate_never_disturbs_the_previously_active_container() {
    if docker_support::skip_unless_enabled() {
        return;
    }

    docker_support::wait_for_vm1_ready(Duration::from_secs(60));

    let dir = tempfile::tempdir().expect("create tempdir");
    let project = "jijidockerrolling";

    let healthy_config = docker_support::write_rolling_deploy_config(dir.path(), project, true);
    let setup = docker_support::run_jiji(&healthy_config, &["server", "setup", "-y"]);
    assert!(
        setup.status.success(),
        "jiji server setup failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&setup.stdout),
        String::from_utf8_lossy(&setup.stderr),
    );

    let first_deploy = docker_support::run_jiji(&healthy_config, &["deploy", "-y"]);
    assert!(
        first_deploy.status.success(),
        "first jiji deploy failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&first_deploy.stdout),
        String::from_utf8_lossy(&first_deploy.stderr),
    );
    docker_support::wait_for_http_ok(
        docker_support::ROLLING_TEST_HOST,
        "/",
        Duration::from_secs(30),
    );

    let running_before = docker_support::all_container_ids_for_service("web");
    assert_eq!(
        running_before.len(),
        1,
        "expected exactly one container for service 'web' after the first deploy, got: {running_before:?}"
    );

    let broken_config = docker_support::write_rolling_deploy_config(dir.path(), project, false);
    let second_deploy = docker_support::run_jiji(&broken_config, &["deploy", "-y"]);
    assert!(
        !second_deploy.status.success(),
        "expected the second jiji deploy (broken healthcheck) to fail, but it succeeded:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&second_deploy.stdout),
        String::from_utf8_lossy(&second_deploy.stderr),
    );

    docker_support::wait_for_http_ok(
        docker_support::ROLLING_TEST_HOST,
        "/",
        Duration::from_secs(5),
    );

    let running_after = docker_support::all_container_ids_for_service("web");
    assert_eq!(
        running_after, running_before,
        "expected the exact same container to still be running after the failed deploy, previous: {running_before:?}, after: {running_after:?}"
    );

    let remove =
        docker_support::run_jiji(&healthy_config, &["service", "remove", "-y", "-S", "web"]);
    assert!(
        remove.status.success(),
        "jiji service remove failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&remove.stdout),
        String::from_utf8_lossy(&remove.stderr),
    );

    let teardown = docker_support::run_jiji(&healthy_config, &["server", "teardown", "-y"]);
    assert!(
        teardown.status.success(),
        "jiji server teardown failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&teardown.stdout),
        String::from_utf8_lossy(&teardown.stderr),
    );
}
