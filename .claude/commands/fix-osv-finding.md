Remediate a vulnerability reported by `mise scan`.

Scanner findings are blockers -- they must be resolved before committing.

## Try to fix first

1. Update the affected dependency in the relevant crate's `Cargo.toml` (or the
   workspace `Cargo.toml` if it's a shared dependency)
2. If it's a transitive dependency, check if upgrading the parent crate
   resolves it (`cargo update -p <crate>`)
3. Run `cargo update` (or `cargo update -p <crate>`) to regenerate `Cargo.lock`
4. Re-run `mise scan` to confirm the finding is resolved

## If the vuln is unreachable

If the vulnerability genuinely does not apply to this project's usage (the
affected code path is never reached, the preconditions don't hold), add an entry
to `osv-scanner.toml`:

```toml
[[IgnoredVulns]]
id = "GHSA-xxxx-xxxx-xxxx"
reason = "discovery doesn't use the affected X feature because Y"
```

The reason must explain why the vuln is unreachable in jiji's architecture
specifically -- not "blocked on upstream" or "toolchain-level". Never dismiss
scanner output without analyzing whether the vuln is reachable.
