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
//! Confirmed live, and asserted on below rather than assumed: dropping a host from a service's
//! own `servers:` list (while that host stays in the project's top-level `servers:`, still fully
//! reachable) does NOT sweep that host's stale cron spec. `cron_reconcile.rs::
//! reconcile_service_crons` builds its sweep session set from `&service.servers` -- the service's
//! own *current* list -- so once a host is no longer in it, the sweep loop structurally never
//! visits it again, regardless of anything still installed there. AGENTS.md's own description of
//! this sweep ("left by a previous owner after an ownership transfer") reads as if this exact
//! case should be covered; in practice it silently isn't. This is a real, current gap, not a
//! flaky assertion -- see AGENTS.md's "Docker-in-Docker integration suite" section for more.
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
    // Confirmed live (see the module doc comment): vm1 is still in the project's top-level
    // `servers:`, fully reachable, but no longer in `web`'s own `servers:` list -- the sweep in
    // `reconcile_service_crons` only ever visits `&service.servers`, so it structurally cannot
    // reach vm1 to remove this. This is real, current jiji behavior, not a bug in this test.
    let specs_vm1_after = docker_support::cron_spec_names_on("vm1", project);
    assert_eq!(
        specs_vm1_after,
        vec!["ping".to_string()],
        "expected vm1's stale 'ping' spec to still be present (dropping a host from a service's \
         own servers: does not sweep it, confirmed live) -- if this now fails, that gap may have \
         been fixed and this assertion should flip to `assert!(specs_vm1_after.is_empty())`"
    );

    let teardown = docker_support::run_jiji(&config_vm2_only, &["server", "teardown", "-y"]);
    assert!(
        teardown.status.success(),
        "jiji server teardown failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&teardown.stdout),
        String::from_utf8_lossy(&teardown.stderr),
    );
}
