---
name: commit
description: Create a git commit following this repo's Conventional Commits + release-please conventions. Use when the user asks to commit, says "commit this", or when reaching a logical stopping point during a task where AGENTS.md's Workflow section calls for committing.
---

# commit

Create a commit following this repo's conventions, and run it. Invoking
this skill is itself the user's explicit go-ahead to commit.

## Before committing

Run the pre-commit-check skill first (mise fmt, lint, test, build, scan).
Every step must pass; fix any failure before staging.

Then check `git status`. If changes are already staged, commit exactly
those staged files; do not stage additional files. If nothing is staged,
stage all changes with `git add -A` and commit them. Before committing
everything, make sure no sensitive files (`.env`, credentials, keys) are
included. If there are no changes at all, say so and do not create an
empty commit.

## Why this matters here specifically

This repo's version/release flow is driven entirely by
[release-please](https://github.com/googleapis/release-please): PRs are
squash-merged, so the **PR title** (usually the commit message, when a PR
is a single commit) is the literal string release-please's
`cargo-workspace` plugin parses to decide whether to bump a version and
what changelog line to write. `.github/workflows/pr-title-lint.yml`
enforces this at the PR level as a backstop; getting the commit message
right here means the PR title is already correct and will not get flagged.

This is also a monorepo with three independently-released binaries
(`jiji`, `jiji-agent`, `jiji-proxy`) plus five internal-only crates that
cascade into them. **Which package(s) a commit bumps is decided by its
changed file paths** (`crates/jiji-cli/**`, `crates/jiji-agent/**`,
`crates/jiji-network/**`, etc.), never by the `(scope)` text in the
message; a scope like `(jiji-cli)` is for human readability in `git log`
only. See `AGENTS.md`'s "Version Management & Releases" section for the
full mechanism (skip-github-release cascade, the internal-crate changelog
expansion script) before touching release plumbing itself.

## Commit message format

```
<type>(<scope>): <description>

[optional body]

[closes #<issue>]
```

## Type rules

Must be one of the types `pr-title-lint.yml` enforces
(`amannn/action-semantic-pull-request`); anything else fails CI on the PR:

- `feat` -- new user-facing feature (minor bump pre-1.0, since
  `bump-minor-pre-major: true`)
- `fix` -- user-facing bug fix only (patch bump). Never use `fix:` or
  `fix(test):` for a commit that only repairs a broken test
- `refactor` -- code change that neither fixes a bug nor adds a feature
- `perf` -- a performance improvement
- `test` or `chore(test)` -- test-only changes
- `ci` -- CI workflow changes (not `fix:`)
- `build` -- build system / dependency changes (e.g. Cargo.toml, build.rs)
- `docs` -- documentation changes
- `chore` -- maintenance, tooling, anything not covered above
- `revert` -- reverts a previous commit

Append `!` after the type/scope (e.g. `fix!:`) for a breaking change;
this bumps the minor version pre-1.0 (`bump-patch-for-minor-pre-major` is
not set) and the major version once past 1.0.

## Scope conventions

- Prefer a crate name as scope when a change is crate-scoped (e.g.
  `fix(jiji-agent): ...`), but remember this is cosmetic; release-please
  attributes the bump by changed path, not by this string.
- Reference the issue number: `closes #42`, `fixes #42`.
- AGENTS.md only commits: add the standard no-ci marker to the
  message. There's nothing to build or test.
- Do not amend unless explicitly requested.
- Do not use `--no-verify` under any circumstances.
- Group logically related changes in one commit; do not bundle unrelated
  changes or split one change across commits.

## Avoid CI trigger phrases

The tokens `[skip ci]`, `[ci skip]`, `[no ci]`, `[skip actions]`, and
`[actions skip]` suppress CI runs. Never include them in commit messages
unless you intend to suppress CI (e.g. AGENTS.md only changes).
When referring to the mechanism, paraphrase instead of writing the literal
token.

## Output

Run `git commit` with the composed message (e.g. `git commit -m "<type>(<scope>): <description>"`,
or `-m`/`-m` pairs for a subject + body) via Bash, then report back the
resulting commit (`git log -1 --oneline` or equivalent); do not just print
the command for the user to run themselves.
