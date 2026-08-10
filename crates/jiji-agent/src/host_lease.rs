//! Same-host, non-blocking advisory lock for host-global (not project-scoped) resources shared by
//! every co-resident project's agent -- currently just jiji-proxy/ingress (Phase 9, see
//! `proxy_bringup.rs`). Every agent sharing this lease is on the same physical host, so a plain
//! local `flock` suffices; there is no network-partition concern the way there would be for the
//! CLI's SSH-driven `HostGlobalProxy` lock scope (`jiji-cli/src/lock.rs`, Phase 7), which
//! coordinates *across* hosts, not within one.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::Path;

pub const DEFAULT_PATH: &str = "/etc/jiji/proxy-ingress/agent.lock";

/// Releases the lease when dropped (closing the fd releases the kernel `flock`).
pub struct HostLeaseGuard {
    _file: File,
}

impl Drop for HostLeaseGuard {
    fn drop(&mut self) {
        // Closing this process's descriptor is insufficient when Podman starts a helper while the
        // lease is held: a helper such as aardvark-dns can inherit a duplicate descriptor and keep
        // the flock alive forever. LOCK_UN releases the shared open-file-description lock before
        // this descriptor closes, including across any inherited duplicate.
        unsafe {
            libc::flock(self._file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

/// Non-blocking: `Ok(None)` means another project's agent currently holds the lease -- expected,
/// routine contention, not an error. Only I/O failures (can't create the lock file's directory,
/// can't open it) are `Err`.
pub fn try_acquire(path: &Path) -> io::Result<Option<HostLeaseGuard>> {
    try_acquire_with(path, is_container_helper)
}

fn try_acquire_with(
    path: &Path,
    inherited_holder: impl Fn(&str) -> bool,
) -> io::Result<Option<HostLeaseGuard>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = open_lock(path)?;
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        Ok(Some(HostLeaseGuard { _file: file }))
    } else {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::WouldBlock {
            if held_only_by_inherited_helpers(&file, &inherited_holder)? {
                // Older agents closed their descriptor without an explicit LOCK_UN. Podman
                // helpers inherited that open-file description and retained its flock forever.
                // Unlinking the stale inode is safe only when no agent, CLI flock process, or
                // unknown process has it open. All current users then coordinate on the new inode.
                drop(file);
                std::fs::remove_file(path)?;
                let replacement = open_lock(path)?;
                let replacement_result =
                    unsafe { libc::flock(replacement.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
                if replacement_result == 0 {
                    return Ok(Some(HostLeaseGuard { _file: replacement }));
                }
                let replacement_error = io::Error::last_os_error();
                if replacement_error.kind() == io::ErrorKind::WouldBlock {
                    return Ok(None);
                }
                return Err(replacement_error);
            }
            Ok(None)
        } else {
            Err(error)
        }
    }
}

fn open_lock(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
}

fn is_container_helper(name: &str) -> bool {
    matches!(
        name,
        "aardvark-dns"
            | "catatonit"
            | "conmon"
            | "crun"
            | "fuse-overlayfs"
            | "pasta"
            | "rootlessport"
            | "slirp4netns"
    )
}

/// An old inherited lock is recoverable only when every other process with the exact inode open
/// is a known container helper. The attempted descriptor in this process is deliberately ignored.
fn held_only_by_inherited_helpers(
    file: &File,
    inherited_holder: &impl Fn(&str) -> bool,
) -> io::Result<bool> {
    let metadata = file.metadata()?;
    let target = (metadata.dev(), metadata.ino());
    let own_pid = std::process::id();
    let mut holders = Vec::new();

    for process in std::fs::read_dir("/proc")? {
        let Ok(process) = process else {
            continue;
        };
        let Some(pid) = process
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == own_pid {
            continue;
        }
        let Ok(descriptors) = std::fs::read_dir(process.path().join("fd")) else {
            continue;
        };
        let mut holds_target = false;
        for descriptor in descriptors.flatten() {
            let Ok(descriptor_metadata) = descriptor.path().metadata() else {
                continue;
            };
            if (descriptor_metadata.dev(), descriptor_metadata.ino()) == target {
                holds_target = true;
                break;
            }
        }
        if !holds_target {
            continue;
        }
        let Ok(name) = std::fs::read_to_string(process.path().join("comm")) else {
            return Ok(false);
        };
        holders.push(name.trim().to_string());
    }

    Ok(!holders.is_empty() && holders.iter().all(|name| inherited_holder(name)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn a_second_non_blocking_acquire_on_the_same_file_is_contended_not_an_error() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("agent.lock");
        let first = try_acquire(&path).expect("first acquire should succeed");
        assert!(first.is_some());
        let second = try_acquire(&path).expect("second acquire should not error");
        assert!(
            second.is_none(),
            "lease should be contended while first is held"
        );
        drop(first);
        let third = try_acquire(&path).expect("third acquire should not error");
        assert!(
            third.is_some(),
            "lease should be free again once the holder is dropped"
        );
    }

    #[test]
    fn dropping_the_guard_unlocks_an_inherited_descriptor() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("agent.lock");
        let guard = try_acquire(&path)
            .expect("acquire lease")
            .expect("lease must be available");
        let inherited_fd = unsafe { libc::dup(guard._file.as_raw_fd()) };
        assert!(inherited_fd >= 0, "duplicate lease descriptor");

        drop(guard);
        let reacquired = try_acquire(&path).expect("reacquire lease after guard drop");

        assert!(
            reacquired.is_some(),
            "an inherited descriptor must not retain the lease"
        );
        unsafe {
            libc::close(inherited_fd);
        }
    }

    #[test]
    fn a_lock_inherited_only_by_a_known_helper_is_recovered() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("agent.lock");
        let legacy = open_lock(&path).expect("open legacy lock");
        assert_eq!(
            unsafe { libc::flock(legacy.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        let inherited = legacy.try_clone().expect("clone legacy descriptor");
        let mut helper = Command::new("sleep")
            .arg("10")
            .stdin(Stdio::from(inherited))
            .spawn()
            .expect("spawn helper with inherited descriptor");
        drop(legacy); // Simulate the old guard, which closed without LOCK_UN.

        let recovered =
            try_acquire_with(&path, |name| name == "sleep").expect("recover inherited helper lock");
        assert!(recovered.is_some());

        helper.kill().expect("stop helper");
        helper.wait().expect("wait for helper");
    }

    #[test]
    fn an_unknown_lock_holder_is_never_recovered() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("agent.lock");
        let legacy = open_lock(&path).expect("open legacy lock");
        assert_eq!(
            unsafe { libc::flock(legacy.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        let inherited = legacy.try_clone().expect("clone legacy descriptor");
        let mut holder = Command::new("sleep")
            .arg("10")
            .stdin(Stdio::from(inherited))
            .spawn()
            .expect("spawn unknown holder");
        drop(legacy);

        let recovered = try_acquire_with(&path, |_| false).expect("inspect unknown holder");
        assert!(recovered.is_none());

        holder.kill().expect("stop holder");
        holder.wait().expect("wait for holder");
    }
}
