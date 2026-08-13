//! Version floors and defaults for the two other jiji binaries this CLI
//! talks to over the network: `jiji-agent` (installed by `jiji server
//! setup`) and `jiji-proxy` (installed/restarted by `jiji server
//! setup`/`jiji proxy restart`). `jiji`, `jiji-agent`, and `jiji-proxy` are
//! versioned and released independently (each its own `[package].version`,
//! its own release-please component and tag) -- a `jiji-cli` release does
//! NOT imply a matching agent/proxy release, and most `jiji` releases touch
//! neither the agent's wire protocol/API nor the proxy's admin protocol, so
//! an operator upgrading `jiji` for an unrelated CLI-only change should not
//! be forced to touch already-healthy infrastructure. Two distinct version
//! references exist here for two distinct purposes (this CLI's own version
//! is `env!("CARGO_PKG_VERSION")`, used directly by `commands::version`,
//! and has no bearing on which agent/proxy build to install):
//!
//! - `AGENT_BUILD_VERSION`: the exact `jiji-agent` version this CLI was
//!   built alongside (read from `crates/jiji-agent/Cargo.toml` at compile
//!   time by `build.rs`, see there) -- the default a fresh `jiji server
//!   setup` installs, since that's the one build guaranteed to exist as a
//!   release and to have been tested against this CLI.
//! - `MIN_AGENT_VERSION`/`MIN_PROXY_VERSION`: the oldest agent/proxy version
//!   this CLI still works against. Deliberately hand-bumped, NOT
//!   automatically tied to any build-time reference: update them only when
//!   a change actually breaks compatibility with an older already-running
//!   agent/proxy.
//!
//! `jiji-proxy` has no equivalent of `AGENT_BUILD_VERSION` here: it's
//! distributed as a container image, not a CLI-downloaded binary, and its
//! own build-time version reference (`jiji_network::proxy_script::
//! PROXY_VERSION`) lives in `jiji-network` instead, since both this CLI and
//! `jiji-agent`'s own native reconcile loop need it -- see that module.

use crate::engine::parse_version;

/// The `jiji-agent` version this CLI was built alongside -- the default a
/// fresh `jiji server setup` installs. See the module docs above and
/// `agent_distribution::managed_download_config`.
pub const AGENT_BUILD_VERSION: &str = env!("JIJI_AGENT_BUILD_VERSION");

/// The oldest jiji-agent/jiji-proxy version this CLI still works against.
/// Update by hand when (and only when) a real compatibility break demands
/// it -- see the module docs above.
pub const MIN_AGENT_VERSION: &str = "0.4.9";
pub const MIN_PROXY_VERSION: &str = "0.4.9";

/// The oldest jiji-agent version that understands `ImageRetentionApply`/`Remove`/`List`.
/// Deliberately a separate, higher floor from `MIN_AGENT_VERSION` rather than bumping that global
/// gate: `MIN_AGENT_VERSION` blocks every RPC on every command, so raising it would refuse to
/// deploy at all against an otherwise-perfectly-compatible older fleet just to gain image
/// retention. `image_retention_reconcile.rs` checks this floor itself, per host, and skips
/// pushing/sweeping retention there (never fails the deploy) when an agent is too old for it.
pub const MIN_RETENTION_AGENT_VERSION: &str = "0.6.5";

/// Fails open (`Ok(())`) on an unparseable `found`/`min`, matching
/// `engine::check_min_version`'s own precedent: never block an operator
/// over a version string this can't understand. `fix_hint` is the exact
/// command that resolves the mismatch (e.g. "Run `jiji server setup` to
/// update it."), since a bare version-mismatch error without one leaves the
/// operator to guess.
pub(crate) fn check_min_version(
    subject: &str,
    host: &str,
    found: &str,
    min: &str,
    fix_hint: &str,
) -> anyhow::Result<()> {
    let (Some(found_version), Some(min_version)) = (parse_version(found), parse_version(min))
    else {
        return Ok(());
    };
    if found_version < min_version {
        anyhow::bail!(
            "{subject} on '{host}' is running v{found}, but this jiji requires at least v{min}. {fix_hint}"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_below_the_floor_is_rejected_with_the_fix_hint() {
        let error = check_min_version(
            "jiji-agent",
            "web1",
            "0.3.0",
            "0.4.9",
            "Run `jiji server setup` to update it.",
        )
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("jiji-agent"));
        assert!(message.contains("web1"));
        assert!(message.contains("0.3.0"));
        assert!(message.contains("0.4.9"));
        assert!(message.contains("jiji server setup"));
    }

    #[test]
    fn an_equal_version_passes() {
        assert!(check_min_version("jiji-proxy", "web1", "0.4.9", "0.4.9", "fix it").is_ok());
    }

    #[test]
    fn a_newer_than_required_version_passes() {
        assert!(check_min_version("jiji-proxy", "web1", "1.0.0", "0.4.9", "fix it").is_ok());
    }

    #[test]
    fn an_unparseable_found_version_fails_open() {
        assert!(
            check_min_version("jiji-agent", "web1", "not-a-version", "0.4.9", "fix it").is_ok()
        );
    }

    #[test]
    fn an_unparseable_min_version_fails_open() {
        assert!(check_min_version(
            "jiji-agent",
            "web1",
            "0.4.9",
            "also-not-a-version",
            "fix it"
        )
        .is_ok());
    }
}
