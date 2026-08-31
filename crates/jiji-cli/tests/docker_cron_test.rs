//! Docker-in-Docker integration suite, scheduled cron: proves `jiji deploy` really installs a
//! cron spec on the owning replica's real `jiji-agent` (`cron_reconcile.rs`'s
//! `reconcile_after_deploy`), and that `jiji service cron run` really executes it in a real
//! one-off container whose output `jiji service cron logs` can then read back, not just that the
//! CLI accepts the request. Reads logs after the run rather than via `run --follow`: an `echo`
//! command finishes near-instantly, and `--follow` lost the race against the container already
//! exiting before it attached in practice (confirmed live) -- `cron logs` reads the latest run's
//! already-recorded output instead, no attach race involved.
//!
//! Requires `JIJI_DOCKER_TESTS=1` and the compose stack already up (`mise test-docker` runs
//! both); otherwise this test skips itself (`docker_support::skip_unless_enabled`).

mod docker_support;

use std::time::Duration;

#[test]
fn scheduled_cron_installs_and_runs_on_the_owning_replica() {
    if docker_support::skip_unless_enabled() {
        return;
    }

    docker_support::wait_for_vm1_ready(Duration::from_secs(60));

    let dir = tempfile::tempdir().expect("create tempdir");
    let project = "jijidockercron";
    let config_path = docker_support::write_config_with_cron_service(dir.path(), project);

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

    let list = docker_support::run_jiji(&config_path, &["service", "cron", "list", "-S", "web"]);
    assert!(
        list.status.success(),
        "jiji service cron list failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&list.stdout),
        String::from_utf8_lossy(&list.stderr),
    );
    let list_stdout = String::from_utf8_lossy(&list.stdout);
    assert!(
        list_stdout.contains("ping"),
        "expected cron 'ping' to be listed as installed after deploy:\n{list_stdout}"
    );

    let run = docker_support::run_jiji(
        &config_path,
        &["service", "cron", "run", "ping", "-S", "web"],
    );
    assert!(
        run.status.success(),
        "jiji service cron run failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );

    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let logs = docker_support::run_jiji(
            &config_path,
            &["service", "cron", "logs", "ping", "-S", "web"],
        );
        let logs_stdout = String::from_utf8_lossy(&logs.stdout);
        if logs.status.success() && logs_stdout.contains(docker_support::CRON_TEST_MARKER) {
            break;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "expected the triggered cron run's own output within 30s, last `cron logs` output:\n{logs_stdout}"
            );
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    let teardown = docker_support::run_jiji(&config_path, &["server", "teardown", "-y"]);
    assert!(
        teardown.status.success(),
        "jiji server teardown failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&teardown.stdout),
        String::from_utf8_lossy(&teardown.stderr),
    );
}
