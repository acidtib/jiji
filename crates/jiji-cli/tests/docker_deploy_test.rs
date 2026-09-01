//! Docker-in-Docker integration suite: `jiji server setup` -> `jiji deploy` of a
//! proxied service -> a real HTTP request through jiji-proxy -> `jiji service remove`, against
//! `vm1` (see `test/docker/compose.yml`). Proves the health-gated deploy transaction and
//! jiji-proxy's nftables ingress actually route real traffic, not just that the right shell
//! commands get rendered.
//!
//! Deliberately uses a stock `nginx:alpine` image rather than `build:`/a local registry, to keep
//! the deploy-transaction/proxy-routing assertions here independent of the build pipeline; see
//! `docker_build_deploy_test.rs` for that, and AGENTS.md's "Docker-in-Docker integration suite"
//! section for the full current coverage.
//!
//! Requires `JIJI_DOCKER_TESTS=1` and the compose stack already up (`mise test-docker` runs
//! both); otherwise this test skips itself (`docker_support::skip_unless_enabled`).

mod docker_support;

use std::time::Duration;

#[test]
fn deploy_routes_real_traffic_through_proxy() {
    if docker_support::skip_unless_enabled() {
        return;
    }

    docker_support::wait_for_vm1_ready(Duration::from_secs(60));

    let dir = tempfile::tempdir().expect("create tempdir");
    let project = "jijidockerdeploy";
    let config_path = docker_support::write_config_with_proxied_web_service(dir.path(), project);

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

    let body = docker_support::wait_for_http_ok(
        docker_support::PROXY_TEST_HOST,
        "/",
        Duration::from_secs(30),
    );
    assert!(
        body.contains("nginx"),
        "expected nginx's default page through jiji-proxy, got:\n{body}"
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
