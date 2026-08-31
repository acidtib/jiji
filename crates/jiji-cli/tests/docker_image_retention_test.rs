//! Docker-in-Docker integration suite, image retention: proves the real local Podman image cache
//! ends up at exactly `retain: N` tags, keeping the most recent ones, not just that the right
//! prune command gets rendered. Confirmed live: `jiji-agent`'s own continuous reconciliation
//! (`image_retention_reconcile.rs`, pushed after every successful deploy) already pruned old tags
//! down to `retain: N` during the deploy loop itself, before the explicit `jiji service prune`
//! call below ever ran -- this only asserts the final state, not which of the two paths did it.
//!
//! Requires `JIJI_DOCKER_TESTS=1` and the compose stack already up (`mise test-docker` runs
//! both); otherwise this test skips itself (`docker_support::skip_unless_enabled`).

mod docker_support;

use std::time::Duration;

#[test]
fn prune_keeps_only_the_configured_retain_count() {
    if docker_support::skip_unless_enabled() {
        return;
    }

    docker_support::wait_for_vm1_ready(Duration::from_secs(60));

    let dir = tempfile::tempdir().expect("create tempdir");
    let project = "jijidockerretain";
    let config_path = docker_support::write_config_with_retained_build_service(dir.path(), project);

    let setup = docker_support::run_jiji(&config_path, &["server", "setup", "-y"]);
    assert!(
        setup.status.success(),
        "jiji server setup failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&setup.stdout),
        String::from_utf8_lossy(&setup.stderr),
    );

    // retain: 2, but deploy 4 distinct versions first so pruning has something real to do. The
    // Dockerfile content must actually change each time: an unchanged Dockerfile builds to the
    // exact same image ID regardless of --version, so all 4 tags would just point at one image
    // that's still in use, with nothing eligible for pruning at all (confirmed live).
    for version in ["v1", "v2", "v3", "v4"] {
        std::fs::write(
            dir.path().join("app/Dockerfile"),
            format!("FROM nginx:alpine\nRUN echo {version} > /version\n"),
        )
        .expect("rewrite test app Dockerfile");
        let deploy = docker_support::run_jiji(
            &config_path,
            &["deploy", "--build", "--version", version, "-y"],
        );
        assert!(
            deploy.status.success(),
            "jiji deploy --build --version {version} failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&deploy.stdout),
            String::from_utf8_lossy(&deploy.stderr),
        );
    }

    // Explicit prune, exercising that command's own code path directly: by this point
    // jiji-agent's own continuous reconciliation may have already pruned old tags on its own
    // (confirmed live), so this may legitimately find nothing left to do -- the assertion below
    // is on the final state, not on this command having done the removing itself.
    let prune = docker_support::run_jiji(&config_path, &["service", "prune", "-S", "web"]);
    assert!(
        prune.status.success(),
        "jiji service prune failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&prune.stdout),
        String::from_utf8_lossy(&prune.stderr),
    );

    let after = docker_support::image_tags_for(project, "web");
    assert_eq!(
        after.len(),
        2,
        "expected exactly 2 image tags remaining after pruning with retain: 2, got: {after:?}"
    );
    assert!(
        after.contains(&"v4".to_string()) && after.contains(&"v3".to_string()),
        "expected the 2 most recently deployed versions (v3, v4) to survive pruning, got: {after:?}"
    );

    let teardown = docker_support::run_jiji(&config_path, &["server", "teardown", "-y"]);
    assert!(
        teardown.status.success(),
        "jiji server teardown failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&teardown.stdout),
        String::from_utf8_lossy(&teardown.stderr),
    );
}
