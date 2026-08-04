//! Same-host, non-blocking advisory lock for host-global (not project-scoped) resources shared by
//! every co-resident project's agent -- currently just kamal-proxy/ingress (Phase 9, see
//! `proxy_bringup.rs`). Every agent sharing this lease is on the same physical host, so a plain
//! local `flock` suffices; there is no network-partition concern the way there would be for the
//! CLI's SSH-driven `HostGlobalProxy` lock scope (`jiji-cli/src/lock.rs`, Phase 7), which
//! coordinates *across* hosts, not within one.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::Path;

pub const DEFAULT_PATH: &str = "/etc/jiji/proxy-ingress/agent.lock";

/// Releases the lease when dropped (closing the fd releases the kernel `flock`).
pub struct HostLeaseGuard {
    _file: File,
}

/// Non-blocking: `Ok(None)` means another project's agent currently holds the lease -- expected,
/// routine contention, not an error. Only I/O failures (can't create the lock file's directory,
/// can't open it) are `Err`.
pub fn try_acquire(path: &Path) -> io::Result<Option<HostLeaseGuard>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        Ok(Some(HostLeaseGuard { _file: file }))
    } else {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::WouldBlock {
            Ok(None)
        } else {
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
