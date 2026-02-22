Run the full pre-commit verification before committing. All steps must pass
before proceeding to `git commit`. Fix any failures inline.

Run these steps in this order (parallelize where independent):

1. `mise fmt` -- Deno formatting checks
2. `mise lint` -- Deno lint checks
3. `mise test` -- Run tests for all packages
4. `mise build` -- Full build (catches type errors and build failures)

Steps 1-2 can run in parallel. Step 3 can run in parallel with step 4.

If `mise fmt` reformats files, re-stage the affected files.

If lint reports issues, fix them manually and re-stage the affected files.

If the build fails due to type errors, fix them before proceeding.

If `mise scan` finds vulnerabilities, try updating the dependency first. Only
add ignore entries if the vuln is genuinely unreachable in this app's code paths
(with a reason explaining why).

Do not proceed to `git commit` until every step passes cleanly. Never use
`--no-verify`. Do NOT run `git commit` -- only run the verification steps.
