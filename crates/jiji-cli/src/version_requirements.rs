//! Minimum-version floors for the two other jiji binaries this CLI talks to
//! over the network: `jiji-agent` (installed by `jiji server setup`) and
//! `jiji-proxy` (installed/restarted by `jiji server setup`/`jiji proxy
//! restart`). Every jiji crate shares one workspace version and is released
//! in lockstep (`workspace.package.version`), but that does NOT mean every
//! `jiji-cli` release requires re-running `jiji server setup`/`jiji proxy
//! restart` -- most releases touch neither the agent's wire protocol/API nor
//! the proxy's admin protocol, so an operator upgrading `jiji` for an
//! unrelated CLI-only change should not be forced to touch already-healthy
//! infrastructure. `MIN_AGENT_VERSION`/`MIN_PROXY_VERSION` are therefore
//! deliberately NOT tied to `env!("CARGO_PKG_VERSION")`: bump them by hand
//! only when a change actually breaks compatibility with an older
//! already-running agent/proxy. `CURRENT_VERSION` is the separate, always-
//! moving constant for "what does a fresh install fetch" -- see
//! `agent_distribution::managed_download_config`.

use crate::engine::parse_version;

/// The exact jiji release this CLI binary was built as. This is what a
/// fresh `jiji server setup` installs and what `jiji proxy restart` pulls --
/// always the release this CLI actually shipped with, since that is the
/// only build guaranteed to exist as a GitHub release and to have been
/// tested against this CLI.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The oldest jiji-agent/jiji-proxy version this CLI still works against.
/// Update by hand when (and only when) a real compatibility break demands
/// it -- see the module docs above.
pub const MIN_AGENT_VERSION: &str = "0.4.9";
pub const MIN_PROXY_VERSION: &str = "0.4.9";

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
