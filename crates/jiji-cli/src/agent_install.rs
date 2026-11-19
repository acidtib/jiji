//! Installs and removes the project-scoped `jiji-agent` systemd service on a host. `jiji server
//! setup` treats a missing local binary as a warning, never a hard failure;
//! `jiji server teardown` always removes whatever was actually installed, the same way it treats
//! every other resource.
//!
//! The binary is uploaded the same way every other jiji-managed file is written remotely
//! (`install -m ... /dev/stdin`, piped over the existing exec channel via
//! `execute_with_input` -- see `proxy_ingress.rs::write_remote_file` for the text-only precedent
//! this generalizes to arbitrary bytes), not SFTP: no new remote-file transport is introduced.

use std::path::{Path, PathBuf};

use jiji_agent::{systemd, AgentPaths};
use jiji_config::ContainerEngine;
use jiji_ssh::SshSession;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    Installed,
    Upgraded,
    AlreadyRunning,
}

fn to_agent_engine(engine: ContainerEngine) -> jiji_agent::Engine {
    match engine {
        ContainerEngine::Docker => jiji_agent::Engine::Docker,
        ContainerEngine::Podman => jiji_agent::Engine::Podman,
    }
}

/// Outcome of resolving a locally built `jiji-agent` binary. `ExplicitOverrideInvalid` is
/// always a hard failure (the caller asked for a specific binary and it isn't there);
/// `NotConfigured` means no override was set and no sibling binary was found next to the
/// running `jiji`, which the caller treats as a fallback signal, not a failure. `Found.explicit`
/// distinguishes an env-var override from the sibling-binary auto-discovery: both still need
/// their *version* checked before being trusted (see `agent_distribution::
/// resolve_agent_binary_source`), but the two should react differently to a stale binary -- an
/// explicit override is worth bailing loudly over, since the caller asked for it specifically;
/// auto-discovery is only ever an optimization over downloading, so it should just fall back.
pub enum LocalAgentBinary {
    Found { path: PathBuf, explicit: bool },
    ExplicitOverrideInvalid(String),
    NotConfigured,
}

/// Resolves the locally built `jiji-agent` binary to upload. `JIJI_AGENT_BINARY` overrides for
/// tests and custom installs, and an invalid override is always reported as
/// `ExplicitOverrideInvalid`, never silently treated as "not configured"; otherwise looks next
/// to the currently running executable, mirroring how `cargo build` places
/// `jiji`/`jiji_dev`/`jiji-agent` in the same target directory. Only checks existence, not
/// version -- see `agent_distribution::resolve_agent_binary_source` for the version gate every
/// `Found` result still has to pass before it's actually used.
pub fn find_local_agent_binary() -> LocalAgentBinary {
    if let Ok(path) = std::env::var("JIJI_AGENT_BINARY") {
        let path = PathBuf::from(path);
        return if path.is_file() {
            LocalAgentBinary::Found {
                path,
                explicit: true,
            }
        } else {
            LocalAgentBinary::ExplicitOverrideInvalid(format!(
                "JIJI_AGENT_BINARY={} does not exist",
                path.display()
            ))
        };
    }
    let candidate = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("jiji-agent")));
    match candidate {
        Some(candidate) if candidate.is_file() => LocalAgentBinary::Found {
            path: candidate,
            explicit: false,
        },
        _ => LocalAgentBinary::NotConfigured,
    }
}

/// Idempotent: skips re-uploading the binary when the remote file's hash already matches (same
/// fingerprint-and-skip shape as `engine::ensure_engine`'s version check), but always
/// re-renders and re-installs the unit file and re-runs `enable --now` so a config-only change
/// (e.g. switching container engines) still takes effect.
///
/// `binary_path: None` means the binary was already installed on the host (spike: the
/// release-download install script did it), so the upload/hash step is skipped entirely and
/// only the config/start tail runs.
pub async fn ensure_agent(
    session: &SshSession,
    engine: ContainerEngine,
    project: &str,
    binary_path: Option<&Path>,
    mesh_config: &jiji_agent::runtime::MeshConfig,
    membership: &[jiji_agent::membership::MembershipRecord],
) -> anyhow::Result<AgentStatus> {
    let paths = AgentPaths::default_for_project(project);

    let bin_dir = paths
        .binary_path
        .parent()
        .expect("binary path always has a parent directory");
    let setup = format!(
        "install -d -m 0700 {} {} {}",
        paths.project_dir.display(),
        bin_dir.display(),
        paths.state_dir.display(),
    );
    run_required(session, &setup, "create the jiji agent's directories").await?;

    let was_active = is_unit_active(session, &paths.unit_name).await?;
    let uploaded = match binary_path {
        Some(binary_path) => {
            let binary = tokio::fs::read(binary_path).await.map_err(|error| {
                anyhow::anyhow!("could not read {}: {error}", binary_path.display())
            })?;
            let local_hash = hex_sha256(&binary);
            let remote_hash = remote_sha256(session, &paths.binary_path).await?;
            let uploaded = remote_hash.as_deref() != Some(local_hash.as_str());
            if uploaded {
                let staged = staged_binary_path(&paths.binary_path);
                write_remote_bytes(session, "0755", &staged, &binary).await?;
                run_required(
                    session,
                    &format!(
                        "{SELF_TEST_FUNCTION}{}",
                        promote_staged_binary_command(&paths.binary_path)
                    ),
                    "install the new jiji agent binary",
                )
                .await?;
            }
            uploaded
        }
        None => false,
    };

    let unit = systemd::render_unit(&paths, project, to_agent_engine(engine));
    write_remote_text(
        session,
        "0600",
        &paths.mesh_config_path,
        &serde_json::to_string_pretty(mesh_config)?,
    )
    .await?;
    write_remote_text(
        session,
        "0600",
        &paths.membership_bootstrap_path,
        &serde_json::to_string(membership)?,
    )
    .await?;
    write_remote_text(session, "0644", &paths.unit_path, &unit).await?;

    let result = session
        .execute(&start_agent_command(&paths, project))
        .await?;
    if !result.success {
        anyhow::bail!(
            "Could not start the jiji agent on {}: {}. Fix the host error and retry the command.",
            session.host(),
            result.stderr.trim()
        );
    }
    if result
        .stdout
        .lines()
        .any(|line| line.trim() == ROLLED_BACK_MARKER)
    {
        anyhow::bail!(
            "The new jiji agent binary did not start on {}; the previous binary was restored and \
             is running again. Check `journalctl -u {}` on that host and report the failure at \
             https://github.com/acidtib/jiji/issues.",
            session.host(),
            paths.unit_name
        );
    }

    Ok(if !was_active {
        AgentStatus::Installed
    } else if uploaded {
        AgentStatus::Upgraded
    } else {
        AgentStatus::AlreadyRunning
    })
}

const ROLLED_BACK_MARKER: &str = "JIJI_AGENT_ROLLED_BACK=1";

/// Where a new binary is written before its self-test; next to the live binary, since `/tmp` may
/// be mounted `noexec`.
pub(crate) fn staged_binary_path(binary: &Path) -> PathBuf {
    PathBuf::from(format!("{}.new", binary.display()))
}

/// The binary a just-replaced agent is rolled back to. It exists only between a swap and the
/// first successful start after it.
pub(crate) fn previous_binary_path(binary: &Path) -> PathBuf {
    PathBuf::from(format!("{}.previous", binary.display()))
}

/// Defines `jiji_agent_self_test BINARY`, which opens a throwaway SQLite store with it.
/// `version` alone is not enough: it never reaches the bundled C code.
pub(crate) const SELF_TEST_FUNCTION: &str = "jiji_agent_self_test() { \
jiji_self_test_dir=$(mktemp -d) || return 1; \
if \"$1\" membership-export --state-dir \"$jiji_self_test_dir\" >/dev/null 2>\"$jiji_self_test_dir/stderr\"; \
then rm -rf \"$jiji_self_test_dir\"; return 0; else jiji_self_test_status=$?; fi; \
jiji_self_test_detail=$(tail -n 3 \"$jiji_self_test_dir/stderr\"); \
rm -rf \"$jiji_self_test_dir\"; \
if [ \"$jiji_self_test_status\" -eq 132 ]; then \
jiji_self_test_detail=\"illegal instruction: this build needs a newer CPU than this host has\"; fi; \
echo \"jiji-agent self-test failed for $1 (exit $jiji_self_test_status): $jiji_self_test_detail\" >&2; \
return 1; }; ";

/// Self-tests the staged binary, then swaps it in and keeps the old one as `.previous`. A failed
/// self-test leaves the installed binary untouched. Needs `SELF_TEST_FUNCTION` defined first.
pub(crate) fn promote_staged_binary_command(binary: &Path) -> String {
    format!(
        "if ! jiji_agent_self_test {staged}; then rm -f {staged}; \
         echo 'The installed jiji-agent binary was left unchanged.' >&2; exit 1; fi; \
         if [ -f {binary} ]; then cp -p {binary} {previous}; fi; \
         mv -f {staged} {binary}",
        staged = staged_binary_path(binary).display(),
        binary = binary.display(),
        previous = previous_binary_path(binary).display(),
    )
}

/// Imports membership and starts the unit, clearing a start-limit `failed` state first. If the
/// agent does not answer a ping and a `.previous` binary exists, restores it, starts again, and
/// prints `ROLLED_BACK_MARKER`.
fn start_agent_command(paths: &AgentPaths, project: &str) -> String {
    format!(
        "systemctl stop {unit} >/dev/null 2>&1 || true; \
         jiji_agent_start() {{ \
         {binary} membership-import --project {project} --state-dir {state} \
         --mesh-config {config} --input {bootstrap}; \
         systemctl daemon-reload; systemctl reset-failed {unit} >/dev/null 2>&1; \
         systemctl enable --now {unit} >/dev/null; \
         for attempt in 1 2 3 4 5; do {binary} ping --socket {socket} >/dev/null \
         && return 0; sleep 1; done; return 1; }}; \
         if jiji_agent_start; then rm -f {previous}; exit 0; fi; \
         if [ -f {previous} ]; then systemctl stop {unit} >/dev/null 2>&1 || true; \
         mv -f {previous} {binary}; \
         if jiji_agent_start; then echo {marker}; exit 0; fi; fi; \
         journalctl -u {unit} -n 10 --no-pager -o cat >&2 || true; exit 1",
        unit = paths.unit_name,
        binary = paths.binary_path.display(),
        previous = previous_binary_path(&paths.binary_path).display(),
        state = paths.state_dir.display(),
        config = paths.mesh_config_path.display(),
        bootstrap = paths.membership_bootstrap_path.display(),
        socket = paths.socket_path.display(),
        marker = ROLLED_BACK_MARKER,
    )
}

/// Stops, disables, and removes the agent's unit, binary, and state -- scoped to this project's
/// own `AgentPaths` only (never a glob over `/etc/jiji/agent`), matching how every other
/// `server teardown` step resolves exact project-derived paths rather than enumerating siblings.
pub async fn remove_agent(session: &SshSession, project: &str) -> anyhow::Result<bool> {
    let paths = AgentPaths::default_for_project(project);
    let was_present = is_unit_active(session, &paths.unit_name).await?
        || path_exists(session, &paths.unit_path).await?;

    let command = format!(
        "systemctl disable --now {unit} >/dev/null 2>&1 || true; \
         rm -f {unit_path}; systemctl daemon-reload; rm -rf {project_dir}",
        unit = paths.unit_name,
        unit_path = paths.unit_path.display(),
        project_dir = paths.project_dir.display(),
    );
    run_required(session, &command, "remove the jiji agent").await?;
    Ok(was_present)
}

async fn is_unit_active(session: &SshSession, unit: &str) -> anyhow::Result<bool> {
    let result = session
        .execute(&format!("systemctl is-active --quiet {unit}"))
        .await?;
    Ok(result.success)
}

async fn path_exists(session: &SshSession, path: &Path) -> anyhow::Result<bool> {
    let result = session
        .execute(&format!("test -e {}", path.display()))
        .await?;
    Ok(result.success)
}

async fn remote_sha256(session: &SshSession, path: &Path) -> anyhow::Result<Option<String>> {
    let command = format!("sha256sum {} 2>/dev/null | cut -d' ' -f1", path.display());
    let result = session.execute(&command).await?;
    let hash = result.stdout.trim();
    Ok((result.success && !hash.is_empty()).then(|| hash.to_string()))
}

pub(crate) fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

async fn write_remote_bytes(
    session: &SshSession,
    mode: &str,
    path: &Path,
    content: &[u8],
) -> anyhow::Result<()> {
    let command = format!("install -D -m {mode} /dev/stdin {}", path.display());
    let result = session.execute_with_input(&command, content).await?;
    if !result.success {
        anyhow::bail!(
            "Could not write {} on {}: {}",
            path.display(),
            session.host(),
            result.stderr.trim()
        );
    }
    Ok(())
}

async fn write_remote_text(
    session: &SshSession,
    mode: &str,
    path: &Path,
    content: &str,
) -> anyhow::Result<()> {
    write_remote_bytes(session, mode, path, content.as_bytes()).await
}

async fn run_required(session: &SshSession, command: &str, action: &str) -> anyhow::Result<()> {
    let result = session.execute(command).await?;
    if !result.success {
        anyhow::bail!(
            "Could not {action} on {}: {}. Fix the host error and retry the command.",
            session.host(),
            result.stderr.trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    fn write_script(path: &Path, body: &str) {
        std::fs::write(path, format!("#!/bin/sh\n{body}\n")).expect("write script");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod script");
    }

    fn run_sh(script: &str, path_prefix: Option<&Path>) -> std::process::Output {
        let mut command = Command::new("sh");
        command.arg("-c").arg(script);
        if let Some(prefix) = path_prefix {
            let path = std::env::var("PATH").unwrap_or_default();
            command.env("PATH", format!("{}:{path}", prefix.display()));
        }
        command.output().expect("run sh")
    }

    fn promote(binary: &Path) -> std::process::Output {
        run_sh(
            &format!(
                "{SELF_TEST_FUNCTION}{}",
                promote_staged_binary_command(binary)
            ),
            None,
        )
    }

    #[test]
    fn a_staged_binary_that_passes_the_self_test_replaces_the_installed_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let binary = dir.path().join("jiji-agent");
        write_script(&binary, "echo old");
        write_script(&staged_binary_path(&binary), "echo new");

        let output = promote(&binary);

        assert!(output.status.success(), "{output:?}");
        assert!(std::fs::read_to_string(&binary)
            .unwrap()
            .contains("echo new"));
        assert!(std::fs::read_to_string(previous_binary_path(&binary))
            .unwrap()
            .contains("echo old"));
        assert!(!staged_binary_path(&binary).exists());
    }

    #[test]
    fn a_staged_binary_that_dies_with_sigill_never_replaces_the_installed_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let binary = dir.path().join("jiji-agent");
        write_script(&binary, "echo old");
        write_script(&staged_binary_path(&binary), "kill -ILL $$");

        let output = promote(&binary);

        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("exit 132"), "{stderr}");
        assert!(stderr.contains("needs a newer CPU"), "{stderr}");
        assert!(stderr.contains("left unchanged"), "{stderr}");
        assert!(std::fs::read_to_string(&binary)
            .unwrap()
            .contains("echo old"));
        assert!(!previous_binary_path(&binary).exists());
        assert!(!staged_binary_path(&binary).exists());
    }

    /// Stub `systemctl`/`journalctl` and a fake agent whose `ping` succeeds only when the binary
    /// contains `healthy`.
    fn start_fixture(new_binary_healthy: bool) -> (tempfile::TempDir, AgentPaths) {
        let dir = tempfile::tempdir().expect("tempdir");
        let stubs = dir.path().join("stubs");
        std::fs::create_dir_all(&stubs).expect("stub dir");
        write_script(&stubs.join("systemctl"), "exit 0");
        write_script(&stubs.join("journalctl"), "echo journal-tail");
        write_script(&stubs.join("sleep"), "exit 0");

        let paths = AgentPaths::for_project("demo", &dir.path().join("agent"));
        std::fs::create_dir_all(paths.binary_path.parent().unwrap()).expect("bin dir");
        let agent = |healthy: bool| {
            format!(
                "# {}\nif [ \"$1\" = ping ]; then exit {}; fi\nexit 0",
                if healthy { "healthy" } else { "broken" },
                if healthy { 0 } else { 1 }
            )
        };
        write_script(&paths.binary_path, &agent(new_binary_healthy));
        write_script(&previous_binary_path(&paths.binary_path), &agent(true));
        (dir, paths)
    }

    #[test]
    fn a_new_agent_that_starts_clears_the_rollback_binary() {
        let (dir, paths) = start_fixture(true);
        let output = run_sh(
            &start_agent_command(&paths, "demo"),
            Some(&dir.path().join("stubs")),
        );

        assert!(output.status.success(), "{output:?}");
        assert!(!String::from_utf8_lossy(&output.stdout).contains(ROLLED_BACK_MARKER));
        assert!(!previous_binary_path(&paths.binary_path).exists());
    }

    #[test]
    fn a_new_agent_that_does_not_start_is_rolled_back_to_the_previous_binary() {
        let (dir, paths) = start_fixture(false);
        let output = run_sh(
            &start_agent_command(&paths, "demo"),
            Some(&dir.path().join("stubs")),
        );

        assert!(output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains(ROLLED_BACK_MARKER));
        assert!(std::fs::read_to_string(&paths.binary_path)
            .unwrap()
            .contains("healthy"));
        assert!(!previous_binary_path(&paths.binary_path).exists());
    }

    #[test]
    fn an_agent_that_does_not_start_without_a_rollback_binary_fails() {
        let (dir, paths) = start_fixture(false);
        std::fs::remove_file(previous_binary_path(&paths.binary_path)).unwrap();
        let output = run_sh(
            &start_agent_command(&paths, "demo"),
            Some(&dir.path().join("stubs")),
        );

        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("journal-tail"));
    }

    #[test]
    fn the_start_command_clears_a_failed_unit_before_starting_it() {
        let paths = AgentPaths::default_for_project("demo");
        let command = start_agent_command(&paths, "demo");
        let reset = command
            .find(&format!("systemctl reset-failed {}", paths.unit_name))
            .expect("reset-failed present");
        let start = command
            .find(&format!("systemctl enable --now {}", paths.unit_name))
            .expect("enable present");
        assert!(reset < start);
    }
}
