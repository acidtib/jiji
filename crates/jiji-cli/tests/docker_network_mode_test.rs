//! Docker-in-Docker integration suite, container namespace scenarios: `network_mode: host` and
//! `network_mode: service:<name>` are the two shapes the mock-SSH suite can only verify by
//! rendered command text (`NetworkedContainerRun::args()`), never by actually inspecting what
//! namespace a real container ended up in. This deploys both against `vm1` in one project and
//! inspects the real Podman state afterward.
//!
//! Requires `JIJI_DOCKER_TESTS=1` and the compose stack already up (`mise test-docker` runs
//! both); otherwise this test skips itself (`docker_support::skip_unless_enabled`).

mod docker_support;

use std::time::Duration;

#[test]
fn host_mode_and_shared_namespace_dependent_deploy_with_real_container_state() {
    if docker_support::skip_unless_enabled() {
        return;
    }

    docker_support::wait_for_vm1_ready(Duration::from_secs(60));

    let dir = tempfile::tempdir().expect("create tempdir");
    let project = "jijidockernetmode";
    let config_path = docker_support::write_config_with_network_mode_services(dir.path(), project);

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

    // network_mode: host -- shares vm1's own network namespace, no bridge address of its own.
    let webhost_id = docker_support::the_container_id_for_service("webhost");
    let webhost_network_mode =
        docker_support::podman_inspect(&webhost_id, ".HostConfig.NetworkMode");
    assert_eq!(
        webhost_network_mode, "host",
        "expected 'webhost' to run with --network host"
    );
    let host_curl =
        docker_support::exec_vm1(&["curl", "-fsS", "--max-time", "5", "http://127.0.0.1:8081/"]);
    assert!(
        host_curl.status.success(),
        "vm1 could not reach the host-mode container directly on its own port 8081: {}",
        String::from_utf8_lossy(&host_curl.stderr)
    );

    // network_mode: service:upstream -- joins 'upstream's namespace instead of getting its own.
    // Comparing SandboxKey (the actual netns file both containers share) is a stronger proof than
    // matching NetworkMode's string formatting: it can only match if they're genuinely the same
    // Linux network namespace, not just similarly configured.
    let upstream_id = docker_support::the_container_id_for_service("upstream");
    let dependent_id = docker_support::the_container_id_for_service("dependent");
    let upstream_sandbox_key =
        docker_support::podman_inspect(&upstream_id, ".NetworkSettings.SandboxKey");
    let dependent_network_mode =
        docker_support::podman_inspect(&dependent_id, ".HostConfig.NetworkMode");
    let dependent_sandbox_key =
        docker_support::podman_inspect(&dependent_id, ".NetworkSettings.SandboxKey");
    assert!(
        dependent_network_mode.starts_with("container:"),
        "expected 'dependent' to run with --network container:<upstream>, got: {dependent_network_mode}"
    );
    assert_eq!(
        dependent_sandbox_key, upstream_sandbox_key,
        "expected 'dependent' and 'upstream' to share the exact same network namespace"
    );

    let teardown = docker_support::run_jiji(&config_path, &["server", "teardown", "-y"]);
    assert!(
        teardown.status.success(),
        "jiji server teardown failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&teardown.stdout),
        String::from_utf8_lossy(&teardown.stderr),
    );
}
