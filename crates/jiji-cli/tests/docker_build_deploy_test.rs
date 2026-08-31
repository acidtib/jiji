//! Phase 2 (build pipeline) of the Docker-in-Docker integration suite: `jiji server setup` ->
//! `jiji deploy --build` of a service built from a real local Dockerfile -> a real HTTP request
//! through jiji-proxy proving the freshly built image (not a cached/stock one) is what's actually
//! running -> `jiji service remove` -> `jiji registry teardown` -> `jiji server teardown`.
//!
//! Closes the gap `docker_deploy_test.rs` deliberately left open: no `registry:` block is
//! configured, so `jiji` manages its own local registry container (see
//! `crates/jiji-cli/src/registry.rs`), builds and pushes the image to it with the real local
//! Podman, and opens a reverse SSH tunnel to vm1 during deploy so vm1's own Podman can pull it
//! back through `localhost:{port}` as if the registry were local to it.
//!
//! Requires `JIJI_DOCKER_TESTS=1`, a local Podman/Docker install (whichever `builder.engine`
//! selects) able to build and run a registry container, and the compose stack already up
//! (`mise test-docker` runs both); otherwise this test skips itself
//! (`docker_support::skip_unless_enabled`).

mod docker_support;

use std::time::Duration;

#[test]
fn deploy_build_pushes_through_local_registry_and_reverse_tunnel() {
    if docker_support::skip_unless_enabled() {
        return;
    }

    docker_support::wait_for_vm1_ready(Duration::from_secs(60));

    let dir = tempfile::tempdir().expect("create tempdir");
    let project = "jijidockerbuild";
    let config_path = docker_support::write_config_with_build_service(dir.path(), project);

    let setup = docker_support::run_jiji(&config_path, &["server", "setup", "-y"]);
    assert!(
        setup.status.success(),
        "jiji server setup failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&setup.stdout),
        String::from_utf8_lossy(&setup.stderr),
    );

    let deploy = docker_support::run_jiji(&config_path, &["deploy", "--build", "-y"]);
    assert!(
        deploy.status.success(),
        "jiji deploy --build failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&deploy.stdout),
        String::from_utf8_lossy(&deploy.stderr),
    );

    let body = docker_support::wait_for_http_ok(
        docker_support::BUILD_PROXY_TEST_HOST,
        "/",
        Duration::from_secs(30),
    );
    assert!(
        body.contains(docker_support::BUILD_TEST_MARKER),
        "expected the freshly built image's own content through jiji-proxy, got:\n{body}"
    );

    let remove = docker_support::run_jiji(&config_path, &["service", "remove", "-y", "-S", "web"]);
    assert!(
        remove.status.success(),
        "jiji service remove failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&remove.stdout),
        String::from_utf8_lossy(&remove.stderr),
    );

    let registry_teardown = docker_support::run_jiji(&config_path, &["registry", "teardown", "-y"]);
    assert!(
        registry_teardown.status.success(),
        "jiji registry teardown failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&registry_teardown.stdout),
        String::from_utf8_lossy(&registry_teardown.stderr),
    );

    let teardown = docker_support::run_jiji(&config_path, &["server", "teardown", "-y"]);
    assert!(
        teardown.status.success(),
        "jiji server teardown failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&teardown.stdout),
        String::from_utf8_lossy(&teardown.stderr),
    );
}
