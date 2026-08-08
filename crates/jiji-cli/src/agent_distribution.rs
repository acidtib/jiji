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

fn env_nonempty(name: &str) -> Option<String> {
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

/// Renders the host-side install script for remote mode: detects the remote arch, downloads the
/// matching artifact and its `.sha256` sidecar from the release, verifies the hash, and installs
/// the binary -- skipping the download when the installed binary already matches (same
/// fingerprint-and-skip shape as `agent_install::ensure_agent`). Mirrors `engine.rs`'s
/// podman-static remote-install precedent.
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
if [ -f {binary} ] && [ \"$(sha256sum {binary} | awk '{{print $1}}')\" = \"$expected\" ]; then exit 0; fi; \
curl -fsSL --retry 3 \"{base}/releases/download/jiji-agent-v{version}/$asset\" -o \"$tmp/$asset\"; \
actual=$(sha256sum \"$tmp/$asset\" | awk '{{print $1}}'); \
if [ \"$expected\" != \"$actual\" ]; then echo \"jiji-agent: sha256 mismatch for $asset (expected $expected, got $actual)\" >&2; exit 1; fi; \
install -d -m 0700 {project_dir} {bin_dir} {state_dir}; \
install -m 0755 \"$tmp/$asset\" {binary}",
        base = base_url,
        version = version,
        project_dir = project_dir.display(),
        bin_dir = bin_dir.display(),
        state_dir = state_dir.display(),
        binary = binary_path.display(),
    )
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
    }
}
