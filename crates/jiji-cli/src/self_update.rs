//! `jiji update`: detect and install a newer `jiji` binary release, mirroring `bin/install.sh`'s
//! detect/download/verify/install behavior (same release repo, same artifact-naming grammar, same
//! checksum algorithm) but through `reqwest`/`serde_json` instead of `curl`/shell, and adding
//! `.sha256` verification that `install.sh` does not perform today.
//!
//! Env overrides (optional, for tests / self-hosted mirrors):
//! - `JIJI_RELEASE_BASE_URL`: asset download base (default: `agent_distribution::DEFAULT_RELEASE_BASE_URL`,
//!   the same override `jiji server setup`'s agent download already uses)
//! - `JIJI_RELEASE_API_BASE_URL`: GitHub API base used only to resolve "latest" (default:
//!   `DEFAULT_RELEASE_API_BASE_URL`). Kept separate from the download base since real GitHub
//!   serves them from different hosts (`github.com` vs `api.github.com`); a test mock server
//!   can still point both at itself.

use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::Context;
use serde::Deserialize;

use crate::agent_distribution::{env_nonempty, DEFAULT_RELEASE_BASE_URL};

pub(crate) const DEFAULT_RELEASE_API_BASE_URL: &str = "https://api.github.com/repos/acidtib/jiji";

pub(crate) fn release_base_url() -> String {
    env_nonempty("JIJI_RELEASE_BASE_URL").unwrap_or_else(|| DEFAULT_RELEASE_BASE_URL.to_string())
}

pub(crate) fn release_api_base_url() -> String {
    env_nonempty("JIJI_RELEASE_API_BASE_URL")
        .unwrap_or_else(|| DEFAULT_RELEASE_API_BASE_URL.to_string())
}

/// Maps `std::env::consts::OS` / `std::env::consts::ARCH` to `jiji-release.yml`'s release
/// artifact names.
pub(crate) fn artifact_name(os: &str, arch: &str) -> anyhow::Result<&'static str> {
    match (os, arch) {
        ("linux", "x86_64") => Ok("jiji-linux-x86_64"),
        ("linux", "aarch64") => Ok("jiji-linux-arm64"),
        ("macos", "x86_64") => Ok("jiji-macos-x86_64"),
        ("macos", "aarch64") => Ok("jiji-macos-arm64"),
        _ => anyhow::bail!(
            "There is no jiji support for {os}/{arch}. Supported: linux/x86_64, linux/arm64, macos/x86_64, macos/arm64."
        ),
    }
}

fn normalize_version_tag(version: &str) -> String {
    if version.starts_with('v') {
        version.to_string()
    } else {
        format!("v{version}")
    }
}

#[derive(Deserialize)]
struct ReleaseListItem {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

/// `true` only for a bare CLI release tag (`v0.8.0`, optionally with a pre-release suffix like
/// `v0.9.0-rc.1`), never one of this repo's other seven crate tags (`jiji-agent-v0.6.4`,
/// `jiji-proxy-v0.6.1`, ...). `release-please-config.json` gives `crates/jiji-cli` alone
/// `"include-component-in-tag": false` -- every other package's tag carries a hyphenated
/// component prefix before the `v`, so the absence of one is what identifies a CLI release.
fn is_cli_release_tag(tag: &str) -> bool {
    let Some(rest) = tag.strip_prefix('v') else {
        return false;
    };
    let core = rest
        .split('-')
        .next()
        .expect("split always yields at least one element");
    let segments: Vec<&str> = core.split('.').collect();
    segments.len() == 3
        && segments
            .iter()
            .all(|segment| !segment.is_empty() && segment.chars().all(|c| c.is_ascii_digit()))
}

/// An explicit `--version` short-circuits with no network call (also the rollback path: it always
/// proceeds even to an older release). Otherwise resolves "latest" from the GitHub release API.
///
/// Deliberately lists releases (`GET /releases`) rather than asking for `/releases/latest`:
/// that endpoint returns the single most recently published release across the *whole repo*, and
/// this repo tags and releases eight crates independently (see `release-please-config.json`) --
/// whichever one happened to release most recently (e.g. `jiji-proxy-v0.6.1`) would otherwise be
/// treated as "the jiji CLI's latest version." `engine::parse_version` skips leading non-digits,
/// so a component-prefixed tag like that still parses to a plausible-looking `(0, 6, 1)` instead
/// of failing loudly, and its download URL then 404s since no `jiji-linux-x86_64` asset was ever
/// published under that tag. Filters to the first non-draft, non-prerelease tag that is actually
/// the CLI's own (`is_cli_release_tag`), matching `/releases`'s newest-first ordering.
/// GitHub paginates `/releases` at 30 entries per page by default. This repo tags and releases
/// 8 crates independently, each with its own tag/release (see `AGENTS.md`'s "Version Management &
/// Releases"), and a `cargo-workspace` patch bump cascades widely -- so enough non-CLI releases
/// (`jiji-agent-v*`, `jiji-core-v*`, ...) landing after the last real CLI tag can push it off a
/// single unpaginated page entirely, making `jiji update` fail to find a CLI release that
/// genuinely exists. `PER_PAGE` is GitHub's own maximum; `MAX_PAGES` bounds the walk so a
/// pathological response (or a broken mock server in tests) can't loop forever.
const RELEASE_LIST_PER_PAGE: u32 = 100;
const RELEASE_LIST_MAX_PAGES: u32 = 20;

pub(crate) async fn resolve_target_version(
    client: &reqwest::Client,
    api_base_url: &str,
    explicit: Option<&str>,
) -> anyhow::Result<String> {
    if let Some(version) = explicit {
        return Ok(normalize_version_tag(version));
    }
    for page in 1..=RELEASE_LIST_MAX_PAGES {
        let url = format!("{api_base_url}/releases?per_page={RELEASE_LIST_PER_PAGE}&page={page}");
        let response = client
            .get(&url)
            .header("User-Agent", "jiji-update")
            .send()
            .await
            .with_context(|| format!("failed to list jiji releases from {url}"))?
            .error_for_status()
            .with_context(|| format!("GitHub returned an error for {url}"))?;
        let releases: Vec<ReleaseListItem> = response
            .json()
            .await
            .with_context(|| format!("could not parse the release list response from {url}"))?;
        let page_len = releases.len();
        if let Some(release) = releases.into_iter().find(|release| {
            !release.draft && !release.prerelease && is_cli_release_tag(&release.tag_name)
        }) {
            return Ok(release.tag_name);
        }
        if page_len < RELEASE_LIST_PER_PAGE as usize {
            // A short (or empty) page means GitHub has no more releases to offer.
            break;
        }
    }
    Err(anyhow::anyhow!(
        "could not find a published jiji CLI release at {api_base_url}/releases (every recent release tag belonged to a different jiji component, or was a draft/prerelease). Check https://github.com/acidtib/jiji/releases for available versions, or pass --release <version> explicitly."
    ))
}

/// `target <= installed`, both compared as parsed semver via `engine::parse_version` (which
/// already skips a leading non-digit, so `v0.8.1` parses unchanged).
pub(crate) fn is_up_to_date(installed: &str, target: &str) -> bool {
    match (
        crate::engine::parse_version(installed),
        crate::engine::parse_version(target),
    ) {
        (Some(installed), Some(target)) => target <= installed,
        _ => false,
    }
}

fn verify_checksum(tag: &str, expected: &str, bytes: &[u8]) -> anyhow::Result<()> {
    let actual = crate::agent_install::hex_sha256(bytes);
    if actual != expected {
        anyhow::bail!(
            "Downloaded artifact for `{tag}` failed checksum verification (expected `{expected}`, got `{actual}`). Try again; if this persists, download manually from https://github.com/acidtib/jiji/releases/tag/{tag}."
        );
    }
    Ok(())
}

fn not_found_error(tag: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "Release `{tag}` was not found. Check https://github.com/acidtib/jiji/releases for available versions."
    )
}

/// Downloads `{asset}.sha256`, then `{asset}`, and verifies the asset's SHA-256 against the
/// sidecar before returning its bytes. A missing sidecar (404) does not fail the download: every
/// release up to and including v0.8.0 predates `.sha256` sidecars being published at all, so
/// treating a 404 there as "release not found" broke `jiji update`/`--release <old tag>` against
/// every release that currently exists. Only the asset itself missing means the release/asset
/// genuinely doesn't exist.
pub(crate) async fn fetch_and_verify_asset(
    client: &reqwest::Client,
    base_url: &str,
    tag: &str,
    asset: &str,
) -> anyhow::Result<Vec<u8>> {
    let sha_url = format!("{base_url}/releases/download/{tag}/{asset}.sha256");
    let sha_response = client
        .get(&sha_url)
        .send()
        .await
        .with_context(|| format!("failed to download {sha_url}"))?;
    let expected = if sha_response.status() == reqwest::StatusCode::NOT_FOUND {
        None
    } else {
        let sha_text = sha_response
            .error_for_status()
            .with_context(|| format!("GitHub returned an error for {sha_url}"))?
            .text()
            .await
            .with_context(|| format!("could not read the checksum body from {sha_url}"))?;
        Some(
            sha_text
                .split_whitespace()
                .next()
                .ok_or_else(|| anyhow::anyhow!("checksum file at {sha_url} was empty"))?
                .to_lowercase(),
        )
    };

    let asset_url = format!("{base_url}/releases/download/{tag}/{asset}");
    let asset_response = client
        .get(&asset_url)
        .send()
        .await
        .with_context(|| format!("failed to download {asset_url}"))?;
    if asset_response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(not_found_error(tag));
    }
    let bytes = asset_response
        .error_for_status()
        .with_context(|| format!("GitHub returned an error for {asset_url}"))?
        .bytes()
        .await
        .with_context(|| format!("could not read the asset body from {asset_url}"))?
        .to_vec();

    match expected {
        Some(expected) => verify_checksum(tag, &expected, &bytes)?,
        None => {
            tracing::warn!(
                %tag,
                "no .sha256 checksum published for this release; installing {asset} without verification"
            );
        }
    }
    Ok(bytes)
}

/// Installs `bytes` at `target_path` atomically: writes into a temp file on the same filesystem
/// (so the final swap is a plain rename, never a partial write at the target), preserving the
/// existing file's permissions. Resolves `target_path` to its canonical form first, so updating a
/// symlinked `jiji` writes through the symlink, matching `install.sh`'s `install` command.
pub(crate) fn install_atomically(target_path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let target_path = fs::canonicalize(target_path)
        .with_context(|| format!("could not resolve {}", target_path.display()))?;
    let parent = target_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has no parent directory", target_path.display()))?;
    let permissions = fs::metadata(&target_path)
        .with_context(|| format!("could not read metadata for {}", target_path.display()))?
        .permissions();

    let mut staged = tempfile::NamedTempFile::new_in(parent).with_context(|| {
        format!(
            "{} is not writable. Re-run with elevated permissions or reinstall jiji to a writable location.",
            parent.display()
        )
    })?;
    staged.write_all(bytes).with_context(|| {
        format!(
            "failed to write the downloaded binary into {}",
            parent.display()
        )
    })?;
    staged
        .as_file()
        .set_permissions(permissions)
        .with_context(|| {
            format!(
                "failed to set permissions on the downloaded binary in {}",
                parent.display()
            )
        })?;
    staged.persist(&target_path).map_err(|error| {
        anyhow::anyhow!(
            "failed to install the new binary at {}: {}",
            target_path.display(),
            error.error
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn artifact_name_maps_supported_combinations() {
        assert_eq!(
            artifact_name("linux", "x86_64").unwrap(),
            "jiji-linux-x86_64"
        );
        assert_eq!(
            artifact_name("linux", "aarch64").unwrap(),
            "jiji-linux-arm64"
        );
        assert_eq!(
            artifact_name("macos", "x86_64").unwrap(),
            "jiji-macos-x86_64"
        );
        assert_eq!(
            artifact_name("macos", "aarch64").unwrap(),
            "jiji-macos-arm64"
        );
    }

    #[test]
    fn artifact_name_rejects_an_unsupported_combination() {
        let error = artifact_name("windows", "x86_64").unwrap_err().to_string();
        assert!(error.contains("windows"));
        assert!(error.contains("x86_64"));
        assert!(error.contains("linux/x86_64"));
    }

    #[test]
    fn is_up_to_date_compares_parsed_versions() {
        assert!(is_up_to_date("v1.2.0", "v1.1.0"));
        assert!(is_up_to_date("v1.2.0", "v1.2.0"));
        assert!(!is_up_to_date("v1.2.0", "v1.3.0"));
    }

    #[test]
    fn normalize_version_tag_adds_a_leading_v_only_when_missing() {
        assert_eq!(normalize_version_tag("0.7.2"), "v0.7.2");
        assert_eq!(normalize_version_tag("v0.7.2"), "v0.7.2");
    }

    #[test]
    fn is_cli_release_tag_accepts_a_bare_version_tag() {
        assert!(is_cli_release_tag("v0.8.0"));
        assert!(is_cli_release_tag("v9.9.9"));
    }

    #[test]
    fn is_cli_release_tag_accepts_a_prerelease_suffix() {
        assert!(is_cli_release_tag("v0.9.0-rc.1"));
    }

    #[test]
    fn is_cli_release_tag_rejects_a_component_prefixed_tag() {
        assert!(!is_cli_release_tag("jiji-agent-v0.6.4"));
        assert!(!is_cli_release_tag("jiji-proxy-v0.6.1"));
        assert!(!is_cli_release_tag("jiji-core-v0.7.0"));
    }

    #[test]
    fn is_cli_release_tag_rejects_garbage() {
        assert!(!is_cli_release_tag("not-a-tag"));
        assert!(!is_cli_release_tag("v1.2"));
        assert!(!is_cli_release_tag(""));
    }

    #[test]
    fn verify_checksum_rejects_a_mismatch() {
        let error = verify_checksum("v0.7.2", "deadbeef", b"hello world")
            .unwrap_err()
            .to_string();
        assert!(error.contains("v0.7.2"));
        assert!(error.contains("deadbeef"));
    }

    #[test]
    fn verify_checksum_accepts_a_match() {
        let expected = crate::agent_install::hex_sha256(b"hello world");
        assert!(verify_checksum("v0.7.2", &expected, b"hello world").is_ok());
    }

    #[test]
    fn install_atomically_preserves_permissions_and_leaves_no_leftover_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("jiji");
        fs::write(&target, b"old").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();

        install_atomically(&target, b"new binary contents").unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"new binary contents");
        let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);

        let entries: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("jiji")]);
    }

    #[test]
    fn install_atomically_leaves_the_original_untouched_on_a_read_only_directory() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("jiji");
        fs::write(&target, b"old").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();

        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o500)).unwrap();
        let result = install_atomically(&target, b"new binary contents");
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();

        assert!(result.is_err());
        assert_eq!(fs::read(&target).unwrap(), b"old");
    }
}
