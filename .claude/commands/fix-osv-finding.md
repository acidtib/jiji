Remediate a vulnerability reported by `mise scan`.

Scanner findings are blockers -- they must be resolved before committing.

## Try to fix first

1. Update the affected dependency in the relevant `deno.json` (root or
   package-level)
2. If it's a transitive npm dependency, check if upgrading the parent package
   resolves it
3. Run `deno install --allow-scripts=npm:cpu-features,npm:ssh2` to regenerate
   the lockfile
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
