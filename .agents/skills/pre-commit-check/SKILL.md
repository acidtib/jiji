---
name: pre-commit-check
description: Run the repo's full pre-commit verification (mise fmt, lint, test, build, scan) and fix every failure. Use when the user asks to run pre-commit checks, or before creating a commit in this repo; every step must pass before `git commit`.
---

# pre-commit-check

Run the full pre-commit verification before committing. All steps must
pass before proceeding to `git commit`. Fix any failures inline.

## Steps

Run these steps in this order (parallelize where independent):

1. `mise fmt` -- cargo fmt
2. `mise lint` -- cargo clippy --all-targets --all-features and cargo fmt --check
3. `mise test` -- run tests
4. `mise build` -- full workspace build (catches type errors and build failures)
5. `mise scan` -- OSV vulnerability scan (findings are blockers)

Steps 1-2 can run in parallel. Steps 3-5 can run in parallel with them.

## Fixing failures

- If `mise fmt` reformats files, re-stage the affected files.
- If `mise lint` reports clippy warnings, treat them as bugs and fix them
  manually, then re-stage the affected files.
- If the build fails due to type errors, fix them before proceeding.
- If `mise scan` finds vulnerabilities, invoke the fix-osv-finding skill
  and remediate every finding through it, then re-stage the affected
  files.

## Scope

AGENTS.md only changes have nothing to build or test; skip steps
3-5 for those.

## Rules

Do not proceed to `git commit` until every step passes cleanly. Never use
`--no-verify`. Do not run `git commit`; only run the verification steps.
