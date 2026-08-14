//! Spike: distributing `jiji-agent` so `jiji server setup` no longer requires an agent binary
//! sitting next to the CLI.
//!
//! Resolution order (`jiji server setup`):
//! 1. `JIJI_AGENT_BINARY` (explicit override, unchanged behavior)
//! 2. a `jiji-agent` binary next to the running `jiji` (dev builds and `mise install`,
//!    unchanged behavior)
//! 3. a download from `jiji-agent`'s own GitHub release (`jiji-agent-v{version}`, `jiji-agent`
//!    being versioned/released independently of this CLI) on each remote host being set up,
//!    verified on the host against the release's `.sha256` sidecar before install.
//!
//! Env overrides (all optional, for tests / self-hosted mirrors / unreleased builds):
//! - `JIJI_AGENT_BINARY`: local binary path, highest priority (pre-existing)
//! - `JIJI_AGENT_VERSION`: version tag to fetch (default:
//!   `version_requirements::AGENT_BUILD_VERSION`, the exact `jiji-agent`
//!   release this CLI was built alongside -- distinct from
//!   `version_requirements::MIN_AGENT_VERSION`, the lower, hand-maintained
//!   floor `agent_client::check_version` enforces against an already-running
//!   agent; see that module's docs for why a fresh install always targets
//!   the build-paired release even though the compatibility floor moves
//!   independently)
//! - `JIJI_RELEASE_BASE_URL`: release base URL (default: `https://github.com/acidtib/jiji`)

use std::path::{Path, PathBuf};

pub const DEFAULT_RELEASE_BASE_URL: &str = "https://github.com/acidtib/jiji";

pub(crate) fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

pub struct ManagedAgentDownload {
    pub version: String,
    pub base_url: String,
}

/// Reads the env-derived settings for the download fallback.
pub fn managed_download_config() -> ManagedAgentDownload {
    ManagedAgentDownload {
        version: env_nonempty("JIJI_AGENT_VERSION")
            .unwrap_or_else(|| crate::version_requirements::AGENT_BUILD_VERSION.to_string()),
        base_url: env_nonempty("JIJI_RELEASE_BASE_URL")
            .unwrap_or_else(|| DEFAULT_RELEASE_BASE_URL.to_string()),
    }
}

pub enum AgentBinarySource {
    Local(PathBuf),
    Managed(ManagedAgentDownload),
}

/// A locally discovered `jiji-agent` binary's version, relative to
/// `version_requirements::AGENT_BUILD_VERSION`. `Unknown` covers both an unparseable version
/// string and a binary old enough to predate the `version` subcommand entirely (its invocation
/// fails outright) -- neither can be proven current, so both are treated the same as `Outdated`
/// by every caller.
enum LocalAgentVersion {
    Current,
    Outdated(String),
    Unknown,
}

/// Runs `{path} version` locally (a fast, trusted, non-networked call -- no different from
/// shelling out to a local `docker`/`podman`) and compares the result against
/// `AGENT_BUILD_VERSION`. This exists because `jiji update` only ever replaces the `jiji` binary
/// on disk, never the `jiji-agent` sitting beside it (see that command's own docs); without this
/// check, a `jiji-agent` left over from before an update silently gets uploaded again as-is,
/// `ensure_agent`'s hash comparison correctly finds it unchanged, and the run reports the host as
/// "already current" even though it never reached the version this CLI actually requires
/// (confirmed live).
async fn local_agent_binary_version(path: &Path) -> LocalAgentVersion {
    let Ok(result) = crate::local_exec::run_captured(
        &path.display().to_string(),
        &["version".to_string()],
        None,
        None,
    )
    .await
    else {
        return LocalAgentVersion::Unknown;
    };
    let found = result.stdout.trim().to_string();
    if !result.success || found.is_empty() {
        return LocalAgentVersion::Unknown;
    }
    match (
        crate::engine::parse_version(&found),
        crate::engine::parse_version(crate::version_requirements::AGENT_BUILD_VERSION),
    ) {
        (Some(found_semver), Some(required_semver)) if found_semver >= required_semver => {
            LocalAgentVersion::Current
        }
        _ => LocalAgentVersion::Outdated(found),
    }
}

/// Shared by `server setup` and `server upgrade`: resolves where the jiji-agent binary to
/// install comes from (local discovery, explicit env override, or the GitHub release download)
/// and renders the host-side install script for the managed case. `bail_context` and
/// `download_notice` carry each command's own wording for the two user-facing strings.
pub async fn resolve_agent_binary_source(
    project: &str,
    bail_context: &str,
    download_notice: impl FnOnce(&str) -> String,
) -> anyhow::Result<(AgentBinarySource, Option<String>)> {
    let binary_source = match crate::agent_install::find_local_agent_binary() {
        crate::agent_install::LocalAgentBinary::Found { path, explicit } => {
            match local_agent_binary_version(&path).await {
                LocalAgentVersion::Current => AgentBinarySource::Local(path),
                LocalAgentVersion::Outdated(found) if explicit => {
                    anyhow::bail!(
                        "{bail_context}: JIJI_AGENT_BINARY={} is v{found}, but this jiji requires \
                         at least v{}. Rebuild it (`mise install`) or point JIJI_AGENT_BINARY at a \
                         matching build.",
                        path.display(),
                        crate::version_requirements::AGENT_BUILD_VERSION
                    );
                }
                LocalAgentVersion::Unknown if explicit => {
                    anyhow::bail!(
                        "{bail_context}: JIJI_AGENT_BINARY={} did not report its own version (too \
                         old, or not a jiji-agent binary at all). Rebuild it (`mise install`) or \
                         point JIJI_AGENT_BINARY at a matching build.",
                        path.display()
                    );
                }
                LocalAgentVersion::Outdated(found) => {
                    let download = managed_download_config();
                    jiji_tui::Ui::warn(&format!(
                        "Local jiji-agent binary at {} is v{found}, below the v{} this jiji \
                         requires; downloading jiji-agent v{} from the release instead. Run \
                         `mise install` to rebuild it locally and skip this download next time.",
                        path.display(),
                        crate::version_requirements::AGENT_BUILD_VERSION,
                        download.version,
                    ));
                    AgentBinarySource::Managed(download)
                }
                LocalAgentVersion::Unknown => {
                    let download = managed_download_config();
                    jiji_tui::Ui::warn(&format!(
                        "Local jiji-agent binary at {} did not report its own version (too old, \
                         or not a jiji-agent binary at all); downloading jiji-agent v{} from the \
                         release instead. Run `mise install` to rebuild it locally and skip this \
                         download next time.",
                        path.display(),
                        download.version,
                    ));
                    AgentBinarySource::Managed(download)
                }
            }
        }
        crate::agent_install::LocalAgentBinary::ExplicitOverrideInvalid(message) => {
            anyhow::bail!("{bail_context}: {message}");
        }
        crate::agent_install::LocalAgentBinary::NotConfigured => {
            let download = managed_download_config();
            jiji_tui::Ui::say(&download_notice(&download.version), 1);
            AgentBinarySource::Managed(download)
        }
    };
    let remote_install_script = match &binary_source {
        AgentBinarySource::Managed(download) => {
            let paths = jiji_agent::AgentPaths::default_for_project(project);
            let bin_dir = paths
                .binary_path
                .parent()
                .expect("binary path always has a parent directory");
            Some(remote_install_script(
                &download.base_url,
                &download.version,
                &paths.project_dir,
                bin_dir,
                &paths.state_dir,
                &paths.binary_path,
            ))
        }
        AgentBinarySource::Local(_) => None,
    };
    Ok((binary_source, remote_install_script))
}

/// Renders the host-side install script for remote mode: detects the remote arch, downloads the
/// matching artifact and its `.sha256` sidecar from the release, verifies the hash, and installs
/// the binary -- skipping the download when the installed binary already matches (same
/// fingerprint-and-skip shape as `agent_install::ensure_agent`). Mirrors `engine.rs`'s
/// podman-static remote-install precedent.
///
/// Prints `JIJI_AGENT_BINARY_CHANGED=0`/`=1` as its last line of stdout: `ensure_agent` is always
/// called with `binary_path: None` afterward (this script already did any real install work), so
/// its own `uploaded` flag is hardcoded `false` and can't tell a caller whether a binary was
/// actually replaced here or the hash already matched. `commands::server::upgrade` reads this
/// marker instead of trusting `ensure_agent`'s resulting `AgentStatus`, which would otherwise
/// report a freshly-upgraded host as merely "already current" (see its own doc comment).
pub fn remote_install_script(
    base_url: &str,
    version: &str,
    project_dir: &Path,
    bin_dir: &Path,
    state_dir: &Path,
    binary_path: &Path,
) -> String {
    format!(
        "set -eu; \
arch=$(uname -m); \
case \"$arch\" in \
x86_64|amd64) asset=jiji-agent-linux-x86_64 ;; \
aarch64|arm64) asset=jiji-agent-linux-arm64 ;; \
*) echo \"jiji-agent: unsupported architecture '$arch' (expected x86_64/amd64/aarch64/arm64)\" >&2; exit 1 ;; \
esac; \
tmp=$(mktemp -d); \
trap 'rm -rf \"$tmp\"' EXIT; \
curl -fsSL --retry 3 \"{base}/releases/download/jiji-agent-v{version}/$asset.sha256\" -o \"$tmp/$asset.sha256\"; \
expected=$(awk '{{print $1}}' \"$tmp/$asset.sha256\"); \
if [ -f {binary} ] && [ \"$(sha256sum {binary} | awk '{{print $1}}')\" = \"$expected\" ]; then echo JIJI_AGENT_BINARY_CHANGED=0; exit 0; fi; \
curl -fsSL --retry 3 \"{base}/releases/download/jiji-agent-v{version}/$asset\" -o \"$tmp/$asset\"; \
actual=$(sha256sum \"$tmp/$asset\" | awk '{{print $1}}'); \
if [ \"$expected\" != \"$actual\" ]; then echo \"jiji-agent: sha256 mismatch for $asset (expected $expected, got $actual)\" >&2; exit 1; fi; \
install -d -m 0700 {project_dir} {bin_dir} {state_dir}; \
install -m 0755 \"$tmp/$asset\" {binary}; \
echo JIJI_AGENT_BINARY_CHANGED=1",
        base = base_url,
        version = version,
        project_dir = project_dir.display(),
        bin_dir = bin_dir.display(),
        state_dir = state_dir.display(),
        binary = binary_path.display(),
    )
}

/// `true` when `remote_install_script`'s stdout ends with its `JIJI_AGENT_BINARY_CHANGED=1`
/// marker, i.e. it actually downloaded and installed a new binary rather than finding the
/// existing one already matched.
pub fn remote_install_script_changed_binary(stdout: &str) -> bool {
    stdout
        .lines()
        .next_back()
        .map(str::trim)
        .is_some_and(|line| line == "JIJI_AGENT_BINARY_CHANGED=1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_install_script_verifies_and_installs() {
        let script = remote_install_script(
            "https://github.com/acidtib/jiji",
            "0.4.9",
            Path::new("/etc/jiji/agent/demo/bin"),
            Path::new("/etc/jiji/agent/demo/bin"),
            Path::new("/etc/jiji/agent/demo/state"),
            Path::new("/etc/jiji/agent/demo/bin/jiji-agent"),
        );
        assert!(script.contains("jiji-agent-linux-x86_64"));
        assert!(script.contains("jiji-agent-linux-arm64"));
        assert!(
            script.contains("https://github.com/acidtib/jiji/releases/download/jiji-agent-v0.4.9")
        );
        assert!(script.contains("sha256sum"));
        assert!(script.contains("sha256 mismatch"));
        assert!(script.contains("install -m 0755"));
        assert!(script.contains("install -d -m 0700"));
        assert!(script.contains("mktemp -d"));
        assert!(script.contains("trap 'rm -rf"));
        assert!(script.contains("/etc/jiji/agent/demo/bin/jiji-agent"));
        assert!(script.contains("JIJI_AGENT_BINARY_CHANGED=0"));
        assert!(script.contains("JIJI_AGENT_BINARY_CHANGED=1"));
    }

    #[test]
    fn remote_install_script_changed_binary_reads_the_trailing_marker() {
        assert!(remote_install_script_changed_binary(
            "some noise\nJIJI_AGENT_BINARY_CHANGED=1\n"
        ));
        assert!(!remote_install_script_changed_binary(
            "some noise\nJIJI_AGENT_BINARY_CHANGED=0\n"
        ));
        assert!(!remote_install_script_changed_binary(""));
        assert!(!remote_install_script_changed_binary("garbage"));
    }
}
