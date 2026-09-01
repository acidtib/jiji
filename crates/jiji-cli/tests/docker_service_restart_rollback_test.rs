//! Docker-in-Docker integration suite, `service restart`/`service rollback`: every other docker
//! test exercises the shared `deploy_endpoint` transaction only through plain `jiji deploy`
//! reruns. This invokes the two CLI entry points themselves, which AGENTS.md documents as sharing
//! that same primitive but never gets its own real-host coverage: `jiji service rollback
//! --version <tag>` (a zero-downtime cycle onto a specific, already-pushed image, no build, no
//! registry push) and `jiji service restart` (a zero-downtime cycle onto whatever image is
//! already active, no version change).
//!
//! Builds and pushes two genuinely distinct images (distinct Dockerfile content, not just
//! distinct `--version` tags -- an unchanged Dockerfile builds to the same image ID regardless of
//! version, confirmed live) through jiji's own local registry, same mechanism as
//! `docker_build_deploy_test.rs`. Rolling back to the first tag needs no live reverse tunnel: the
//! image is already resident in vm1's own Podman cache from the first build (`ensure_image` only
//! pulls a missing image), which is what makes exercising rollback safe without re-opening the
//! tunnel `deploy --build` used.
//!
//! Requires `JIJI_DOCKER_TESTS=1`, a local Podman/Docker install able to build and run a registry
//! container, and the compose stack already up (`mise test-docker` runs both); otherwise this
//! test skips itself (`docker_support::skip_unless_enabled`).

mod docker_support;

use std::time::Duration;

#[test]
fn rollback_reverts_to_a_previously_pushed_version_and_restart_replaces_the_container() {
    if docker_support::skip_unless_enabled() {
        return;
    }

    docker_support::wait_for_vm1_ready(Duration::from_secs(60));

    let dir = tempfile::tempdir().expect("create tempdir");
    let project = "jijidockerrestartrb";

    let config_v1 =
        docker_support::write_config_with_marked_build_service(dir.path(), project, "v1-marker");
    let setup = docker_support::run_jiji(&config_v1, &["server", "setup", "-y"]);
    assert!(
        setup.status.success(),
        "jiji server setup failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&setup.stdout),
        String::from_utf8_lossy(&setup.stderr),
    );

    let deploy_v1 =
        docker_support::run_jiji(&config_v1, &["deploy", "--build", "--version", "v1", "-y"]);
    assert!(
        deploy_v1.status.success(),
        "jiji deploy --build --version v1 failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&deploy_v1.stdout),
        String::from_utf8_lossy(&deploy_v1.stderr),
    );
    let body_v1 = docker_support::wait_for_http_ok(
        docker_support::RESTART_ROLLBACK_TEST_HOST,
        "/",
        Duration::from_secs(60),
    );
    assert!(
        body_v1.contains("v1-marker"),
        "expected v1's own content after the first build, got:\n{body_v1}"
    );
    let container_v1 = docker_support::the_container_id_for_service("web");

    // Same project, same server, a genuinely different Dockerfile: a distinct image under a
    // distinct pushed tag, not a relabeled build of the same content.
    let config_v2 =
        docker_support::write_config_with_marked_build_service(dir.path(), project, "v2-marker");
    let deploy_v2 =
        docker_support::run_jiji(&config_v2, &["deploy", "--build", "--version", "v2", "-y"]);
    assert!(
        deploy_v2.status.success(),
        "jiji deploy --build --version v2 failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&deploy_v2.stdout),
        String::from_utf8_lossy(&deploy_v2.stderr),
    );
    let body_v2 = docker_support::wait_for_http_ok(
        docker_support::RESTART_ROLLBACK_TEST_HOST,
        "/",
        Duration::from_secs(60),
    );
    assert!(
        body_v2.contains("v2-marker"),
        "expected v2's own content after the second build, got:\n{body_v2}"
    );
    let container_v2 = docker_support::the_container_id_for_service("web");
    assert_ne!(
        container_v2, container_v1,
        "expected a genuinely new container after redeploying with a new build"
    );

    let rollback = docker_support::run_jiji(
        &config_v2,
        &["service", "rollback", "-S", "web", "--version", "v1"],
    );
    assert!(
        rollback.status.success(),
        "jiji service rollback --version v1 failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&rollback.stdout),
        String::from_utf8_lossy(&rollback.stderr),
    );
    let body_after_rollback = docker_support::wait_for_http_ok(
        docker_support::RESTART_ROLLBACK_TEST_HOST,
        "/",
        Duration::from_secs(60),
    );
    assert!(
        body_after_rollback.contains("v1-marker"),
        "expected v1's content back after rollback, got:\n{body_after_rollback}"
    );
    let container_after_rollback = docker_support::the_container_id_for_service("web");
    assert_ne!(
        container_after_rollback, container_v2,
        "expected rollback to replace the container, not reuse v2's"
    );

    let restart = docker_support::run_jiji(&config_v2, &["service", "restart", "-S", "web"]);
    assert!(
        restart.status.success(),
        "jiji service restart failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&restart.stdout),
        String::from_utf8_lossy(&restart.stderr),
    );
    let body_after_restart = docker_support::wait_for_http_ok(
        docker_support::RESTART_ROLLBACK_TEST_HOST,
        "/",
        Duration::from_secs(60),
    );
    assert!(
        body_after_restart.contains("v1-marker"),
        "expected restart to reuse the currently active (v1) image unchanged, got:\n{body_after_restart}"
    );
    let container_after_restart = docker_support::the_container_id_for_service("web");
    assert_ne!(
        container_after_restart, container_after_rollback,
        "expected restart to replace the container even though the image didn't change"
    );

    let remove = docker_support::run_jiji(&config_v2, &["service", "remove", "-y", "-S", "web"]);
    assert!(
        remove.status.success(),
        "jiji service remove failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&remove.stdout),
        String::from_utf8_lossy(&remove.stderr),
    );

    let registry_teardown = docker_support::run_jiji(&config_v2, &["registry", "teardown", "-y"]);
    assert!(
        registry_teardown.status.success(),
        "jiji registry teardown failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&registry_teardown.stdout),
        String::from_utf8_lossy(&registry_teardown.stderr),
    );

    let teardown = docker_support::run_jiji(&config_v2, &["server", "teardown", "-y"]);
    assert!(
        teardown.status.success(),
        "jiji server teardown failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&teardown.stdout),
        String::from_utf8_lossy(&teardown.stderr),
    );
}
