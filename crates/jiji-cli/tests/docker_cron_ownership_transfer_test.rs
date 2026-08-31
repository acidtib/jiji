//! Docker-in-Docker integration suite, cron ownership transfer: `docker_cron_test.rs` only
//! covers first installation and a triggered run on the original (and only) owner. This proves
//! the other half AGENTS.md calls out separately: moving an already-installed spec to a new
//! owning replica after a redeploy changes which host is desired for a service.
//!
//! Verifies both the CLI-visible owner (`jiji service cron status`'s `owner=` field) and, more
//! directly, each host's own `jiji-agent` control socket (`RequestBody::CronSpecList`) -- ground
//! truth for what a given agent genuinely has installed, not just what the CLI's "current owner"
//! lookup happens to report.
//!
//! Also verifies a fix for a real gap found via this test: dropping a host from a service's own
//! `servers:` list (while that host stays in the project's top-level `servers:`, still fully
//! reachable) now still sweeps that host's stale cron spec. `cron_reconcile.rs::
//! reconcile_service_crons` used to build its sweep session set from `&service.servers` alone --
//! the service's own *current* list -- so once a host dropped out of it, the sweep loop
//! structurally could never visit it again, regardless of anything still installed there. It now
//! widens the sweep to every server in the project (`config.servers`) whenever the service has
//! `crons:` configured, so a host that used to be eligible is still reached.
//!
//! Requires `JIJI_DOCKER_TESTS=1` and the compose stack already up (`mise test-docker` runs
//! both); otherwise this test skips itself (`docker_support::skip_unless_enabled`).

mod docker_support;

use std::time::Duration;

#[test]
fn cron_ownership_moves_but_a_host_dropped_from_servers_keeps_its_stale_spec() {
    if docker_support::skip_unless_enabled() {
        return;
    }

    docker_support::wait_for_service_ready("vm1", Duration::from_secs(60));
    docker_support::wait_for_service_ready("vm2", Duration::from_secs(60));

    let dir = tempfile::tempdir().expect("create tempdir");
    let project = "jijidockercrontransfer";

    // vm1 sorts before vm2 alphabetically, so with both listed it gets ordinal 0 and owns the
    // cron initially (placement::assignments_for sorts servers, assigns ordinals in that order).
    let config_both = docker_support::write_config_with_transferable_cron_service(
        dir.path(),
        project,
        &["vm1", "vm2"],
    );

    let setup = docker_support::run_jiji(&config_both, &["server", "setup", "-y"]);
    assert!(
        setup.status.success(),
        "jiji server setup failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&setup.stdout),
        String::from_utf8_lossy(&setup.stderr),
    );

    let deploy1 = docker_support::run_jiji(&config_both, &["deploy", "-y"]);
    assert!(
        deploy1.status.success(),
        "first jiji deploy failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&deploy1.stdout),
        String::from_utf8_lossy(&deploy1.stderr),
    );

    let status1 =
        docker_support::run_jiji(&config_both, &["service", "cron", "status", "-S", "web"]);
    assert!(status1.status.success(), "jiji service cron status failed");
    let status1_stdout = String::from_utf8_lossy(&status1.stdout);
    assert!(
        status1_stdout.contains("owner=vm1"),
        "expected vm1 (ordinal 0) to own the cron initially, got:\n{status1_stdout}"
    );
    let specs_vm1_before = docker_support::cron_spec_names_on("vm1", project);
    assert!(
        specs_vm1_before.contains(&"ping".to_string()),
        "expected vm1's own agent to have 'ping' installed while it owns the cron, got: {specs_vm1_before:?}"
    );

    // Drop vm1 from the *service's* own deploy targets. It stays in the project's top-level
    // `servers:`, so jiji can still reach it to sweep the stale spec -- this is what actually
    // moves ownership in the real placement algorithm, not a bare scale-number change alone.
    let config_vm2_only =
        docker_support::write_config_with_transferable_cron_service(dir.path(), project, &["vm2"]);

    let deploy2 = docker_support::run_jiji(&config_vm2_only, &["deploy", "-y"]);
    assert!(
        deploy2.status.success(),
        "second jiji deploy (vm1 dropped from web's servers:) failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&deploy2.stdout),
        String::from_utf8_lossy(&deploy2.stderr),
    );

    let status2 = docker_support::run_jiji(
        &config_vm2_only,
        &["service", "cron", "status", "-S", "web"],
    );
    assert!(
        status2.status.success(),
        "jiji service cron status failed after transfer"
    );
    let status2_stdout = String::from_utf8_lossy(&status2.stdout);
    assert!(
        status2_stdout.contains("owner=vm2"),
        "expected ownership to move to vm2 once vm1 was dropped from web's servers:, got:\n{status2_stdout}"
    );

    let specs_vm2_after = docker_support::cron_spec_names_on("vm2", project);
    assert!(
        specs_vm2_after.contains(&"ping".to_string()),
        "expected vm2's own agent to have 'ping' installed as the new owner, got: {specs_vm2_after:?}"
    );
    // vm1 is still in the project's top-level `servers:`, fully reachable, but no longer in
    // `web`'s own `servers:` list. `reconcile_service_crons` now widens its sweep to every
    // project server whenever `web` has `crons:` configured, so vm1's stale spec is still found
    // and removed even though `web` no longer targets it.
    let specs_vm1_after = docker_support::cron_spec_names_on("vm1", project);
    assert!(
        specs_vm1_after.is_empty(),
        "expected vm1's stale 'ping' spec to be swept once it dropped from web's servers:, got: {specs_vm1_after:?}"
    );

    let teardown = docker_support::run_jiji(&config_vm2_only, &["server", "teardown", "-y"]);
    assert!(
        teardown.status.success(),
        "jiji server teardown failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&teardown.stdout),
        String::from_utf8_lossy(&teardown.stderr),
    );
}
