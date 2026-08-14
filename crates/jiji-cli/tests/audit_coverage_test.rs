//! Static coverage check for `docs/todo.md`'s "Complete audit coverage" item: every command file
//! under `src/commands/` that mutates state reachable over SSH must reference `audit::record`/
//! `audit::record_endpoints_by_server` somewhere in the file. This is a file-level check, not a
//! function-level one -- a file with one audited command and one unaudited helper would pass --
//! but it catches the realistic failure mode this test exists for: a brand new command file added
//! with no audit wiring at all, or an existing one that loses its audit call in a later edit.
//!
//! Every `.rs` file under `src/commands/` must appear in exactly one of the two lists below.
//! `EXPECTED_AUDITED` files are asserted to contain an audit call. `EXCLUDED` files are read-only,
//! local-only (no SSH session to write an audit entry through), an internal helper module (not a
//! standalone CLI command), or a documented pre-existing gap (out of scope for this change,
//! tracked separately). A file in neither list fails the test with a message asking for it to be
//! triaged into one of them -- this is what makes the test catch a genuinely new command.

use std::path::{Path, PathBuf};

/// Command files expected to call `audit::record`/`record_endpoints_by_server` somewhere in the
/// file, because they mutate state on at least one remote host over SSH.
const EXPECTED_AUDITED: &[&str] = &[
    // Only audited on the `builder.remote` path: `BuildExecutor::remote_session` returns `None`
    // for a local build (no `SshSession` opened at all), so this file-level check only proves the
    // `audit::record` call exists somewhere in the file, not that every build in every
    // configuration writes an entry -- a local build has no host to write one through, the same
    // local-only exclusion `registry teardown` uses.
    "build.rs",
    "deploy.rs",
    "lock/acquire.rs",
    "lock/release.rs",
    "network/compact.rs",
    "network/setup.rs",
    // `restore` is the only mutating, remote-touching entry point in this file: `run` is a
    // read/export (writes only a local file) and `recover` is mutating but purely local (no
    // `SshSession` anywhere in that function -- decrypts a local backup file and writes local
    // `.jiji/recovery/` state). This file-level check only proves `restore`'s audit call exists
    // somewhere in the file, not that `run`/`recover` stayed unaudited-by-design on purpose.
    "network/backup.rs",
    "proxy/restart.rs",
    "registry/login.rs",
    "registry/logout.rs",
    "server/exec.rs",
    "server/setup.rs",
    "server/teardown.rs",
    "server/upgrade.rs",
    "service/cron/run.rs",
    "service/prune.rs",
    "service/remove.rs",
    "service/restart.rs",
    "service/rollback.rs",
    "service/scale.rs",
];

/// Everything else, with why it's not held to the same requirement.
const EXCLUDED: &[(&str, &str)] = &[
    ("mod.rs", "module re-export only, not a command"),
    ("network/mod.rs", "module re-export only, not a command"),
    ("registry/mod.rs", "module re-export only, not a command"),
    ("proxy/mod.rs", "module re-export only, not a command"),
    ("server/mod.rs", "module re-export only, not a command"),
    ("service/mod.rs", "module re-export only, not a command"),
    ("lock/mod.rs", "module re-export only, not a command"),
    ("secrets/mod.rs", "module re-export only, not a command"),
    (
        "service/cron/mod.rs",
        "module re-export only, not a command",
    ),
    ("audit.rs", "read-only: the audit trail's own reader"),
    ("version.rs", "read-only, no SSH session at all"),
    ("secrets/print.rs", "read-only"),
    (
        "network/plan.rs",
        "read-only: prints the plan, applies nothing",
    ),
    ("network/catalog.rs", "read-only"),
    ("network/diagnostics.rs", "read-only"),
    (
        "network/assess.rs",
        "internal helper (`pub(crate) assess_host`) invoked by `server setup --import`, not a \
         standalone command; that command's own audit entry covers this",
    ),
    (
        "network/import.rs",
        "internal helper (`pub(crate) run_import`) invoked by `server setup --import`, not a \
         standalone command; that command's own audit entry covers this",
    ),
    (
        "network/membership.rs",
        "internal helpers invoked by `server setup`'s membership reconciliation, not a \
         standalone command; that command's own audit entry covers this",
    ),
    (
        "network/bridge.rs",
        "internal helper invoked by `network setup` and `proxy restart`, not a standalone \
         command; each caller's own audit entry covers this",
    ),
    ("proxy/logs.rs", "read-only: streams container output"),
    (
        "registry/shared.rs",
        "shared helper module for `registry login`/`logout`/`teardown`, not a command itself",
    ),
    (
        "registry/teardown.rs",
        "mutating but purely local: `load_config`, not `load_config_for_ssh`, no `SshSession` \
         anywhere in the file -- there is no host to write a per-server audit entry through",
    ),
    (
        "init.rs",
        "scaffolds a local `.jiji/deploy.yml` only, no SSH session",
    ),
    (
        "update.rs",
        "never touches remote servers (see `jiji update`'s own command reference)",
    ),
    ("lock/show.rs", "read-only"),
    ("lock/status.rs", "read-only"),
    ("service/logs.rs", "read-only: streams container output"),
    ("service/cron/list.rs", "read-only"),
    ("service/cron/status.rs", "read-only"),
    (
        "service/cron/logs.rs",
        "read-only: streams a run's container output",
    ),
];

fn commands_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands")
}

/// Every `.rs` file under `src/commands/`, as a path relative to `src/commands/` itself (e.g.
/// `"network/setup.rs"`), so entries in the two lists above stay short and match how the rest of
/// this codebase already refers to these files.
fn all_command_files() -> Vec<String> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).expect("read commands dir") {
            let entry = entry.expect("read dir entry");
            let path = entry.path();
            if path.is_dir() {
                walk(&path, root, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let relative = path
                    .strip_prefix(root)
                    .expect("path under commands root")
                    .to_str()
                    .expect("valid utf8 path")
                    .to_string();
                out.push(relative);
            }
        }
    }
    let root = commands_dir();
    let mut files = Vec::new();
    walk(&root, &root, &mut files);
    files.sort();
    files
}

fn file_contains_audit_call(relative_path: &str) -> bool {
    let content = std::fs::read_to_string(commands_dir().join(relative_path))
        .unwrap_or_else(|error| panic!("read {relative_path}: {error}"));
    content.contains("audit::record")
}

#[test]
fn every_command_file_is_triaged_as_audited_or_excluded() {
    let files = all_command_files();
    let mut untriaged = Vec::new();
    for file in &files {
        let audited = EXPECTED_AUDITED.contains(&file.as_str());
        let excluded = EXCLUDED.iter().any(|(name, _)| name == file);
        if !audited && !excluded {
            untriaged.push(file.clone());
        }
        assert!(
            !(audited && excluded),
            "{file} is listed in both EXPECTED_AUDITED and EXCLUDED; pick one"
        );
    }
    assert!(
        untriaged.is_empty(),
        "new command file(s) under src/commands/ with no audit-coverage triage: {untriaged:?}. \
         Add each one to EXPECTED_AUDITED (if it mutates state on a remote host) or EXCLUDED \
         (with a reason) in tests/audit_coverage_test.rs."
    );
}

#[test]
fn every_expected_audited_file_actually_calls_audit_record() {
    let mut missing = Vec::new();
    for file in EXPECTED_AUDITED {
        if !file_contains_audit_call(file) {
            missing.push(*file);
        }
    }
    assert!(
        missing.is_empty(),
        "command file(s) expected to write an audit entry but found no `audit::record` \
         reference: {missing:?}"
    );
}

#[test]
fn expected_audited_and_excluded_lists_cover_every_real_file() {
    // Guards against a stale entry in either list outliving the file it names (e.g. a rename),
    // which would otherwise silently stop covering anything.
    let files: std::collections::HashSet<String> = all_command_files().into_iter().collect();
    for file in EXPECTED_AUDITED {
        assert!(
            files.contains(*file),
            "EXPECTED_AUDITED names '{file}', which no longer exists under src/commands/"
        );
    }
    for (file, _) in EXCLUDED {
        assert!(
            files.contains(*file),
            "EXCLUDED names '{file}', which no longer exists under src/commands/"
        );
    }
}
