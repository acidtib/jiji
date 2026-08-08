---
name: fix-osv-finding
description: Remediate vulnerabilities reported by `mise scan` (osv-scanner) in this repo, upgrading affected dependencies, regenerating Cargo.lock, and adding justified osv-scanner.toml ignore entries only for genuinely unreachable findings. Use when `mise scan` reports a finding, when the user asks to fix an OSV finding or invokes /fix-osv-finding, or when pre-commit verification reports a scanner finding.
---

# fix-osv-finding

Scanner findings are blockers: resolve them before committing. Never
dismiss scanner output without analyzing whether the vulnerability is
reachable.

## Fix first

1. Update the affected dependency in the relevant crate's `Cargo.toml`
   (or the workspace `Cargo.toml` if it is a shared dependency).
2. If it is a transitive dependency, check whether upgrading the parent
   crate resolves it (`cargo update -p <crate>`).
3. Run `cargo update` (or `cargo update -p <crate>`) to regenerate
   `Cargo.lock`.

## If the vulnerability is unreachable

If the vulnerability genuinely does not apply to this project's usage
(the affected code path is never reached, or the preconditions do not
hold), add an entry to `osv-scanner.toml`:

```toml
[[IgnoredVulns]]
id = "GHSA-xxxx-xxxx-xxxx"
reason = "discovery does not use the affected X feature because Y"
```

The reason must explain why the vulnerability is unreachable in jiji's
architecture specifically, not "blocked on upstream" or
"toolchain-level".

## Confirm

Re-run `mise scan` after any change and keep going until it passes
cleanly.
