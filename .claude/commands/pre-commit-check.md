Run the full pre-commit verification before committing. All steps must pass
before proceeding to `git commit`. Fix any failures inline.

Run these steps in this order (parallelize where independent):

1. `mise fmt` -- `cargo fmt`
2. `mise lint` -- `cargo clippy --all-targets --all-features && cargo fmt --check`
3. `mise test` -- run tests for every crate in the workspace
4. `mise build` -- full workspace build (catches type errors and build failures)

Steps 1-2 can run in parallel. Step 3 can run in parallel with step 4.

If `mise fmt` reformats files, re-stage the affected files.

If `mise lint` reports clippy warnings, treat them as bugs and fix them
manually, then re-stage the affected files.

If the build fails due to type errors, fix them before proceeding.

If `mise scan` finds vulnerabilities, try updating the dependency first. Only
add ignore entries if the vuln is genuinely unreachable in this app's code paths
(with a reason explaining why).

Do not proceed to `git commit` until every step passes cleanly. Never use
`--no-verify`. Do NOT run `git commit` -- only run the verification steps.
