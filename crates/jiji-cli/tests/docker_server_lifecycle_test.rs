//! Docker-in-Docker integration suite: `jiji server setup` -> real state
//! assertions -> `jiji server teardown`, against `vm1` (see `test/docker/compose.yml`), a
//! privileged container running its own systemd. Nothing here is mocked: Podman is really
//! installed by jiji's own static-binary install path, the `jiji-agent` systemd unit is really
//! started, and the WireGuard interface is really brought up against the host kernel's
//! `wireguard` module.
//!
//! Requires `JIJI_DOCKER_TESTS=1` and the compose stack already up (`mise test-docker` runs
//! both); otherwise this test skips itself (`docker_support::skip_unless_enabled`).

mod docker_support;

use std::time::Duration;

#[test]
fn server_setup_starts_real_agent_and_teardown_removes_it() {
    if docker_support::skip_unless_enabled() {
        return;
    }

    docker_support::wait_for_vm1_ready(Duration::from_secs(60));

    let dir = tempfile::tempdir().expect("create tempdir");
    let project = "jijidockertest";
    let config_path = docker_support::write_config(dir.path(), project);

    let setup = docker_support::run_jiji(&config_path, &["server", "setup", "-y"]);
    assert!(
        setup.status.success(),
        "jiji server setup failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&setup.stdout),
        String::from_utf8_lossy(&setup.stderr),
    );

    let unit = format!(
        "jiji-agent-{}.service",
        jiji_network::systemd_unit_slug(project)
    );
    let iface = jiji_network::wireguard_interface_name(project);

    let podman_info = docker_support::exec_vm1(&["podman", "info"]);
    assert!(
        podman_info.status.success(),
        "podman info failed on vm1 after server setup:\nstderr: {}",
        String::from_utf8_lossy(&podman_info.stderr),
    );

    let unit_status = docker_support::exec_vm1_stdout(&["systemctl", "is-active", &unit]);
    assert_eq!(
        unit_status.trim(),
        "active",
        "expected {unit} to be active on vm1 after server setup"
    );

    let wg_show = docker_support::exec_vm1(&["wg", "show", &iface]);
    assert!(
        wg_show.status.success(),
        "wg show {iface} failed on vm1 after server setup:\nstderr: {}",
        String::from_utf8_lossy(&wg_show.stderr),
    );

    let diagnostics = docker_support::run_jiji(&config_path, &["network", "diagnostics"]);
    assert!(
        diagnostics.status.success(),
        "jiji network diagnostics failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&diagnostics.stdout),
        String::from_utf8_lossy(&diagnostics.stderr),
    );

    let teardown = docker_support::run_jiji(&config_path, &["server", "teardown", "-y"]);
    assert!(
        teardown.status.success(),
        "jiji server teardown failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&teardown.stdout),
        String::from_utf8_lossy(&teardown.stderr),
    );

    assert!(
        !docker_support::exec_vm1_ok(&["systemctl", "is-active", &unit]),
        "expected {unit} to be gone from vm1 after server teardown"
    );
    assert!(
        !docker_support::exec_vm1_ok(&["ip", "link", "show", &iface]),
        "expected {iface} to be gone from vm1 after server teardown"
    );
}
