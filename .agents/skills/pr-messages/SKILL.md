---
name: pr-messages
description: Write a message, summary, description, or release-please-compliant title for an existing GitHub pull request using the gh CLI. Use when the user asks for a PR message, PR title, PR summary, or write-up, optionally providing a PR number. If no PR number is given, detect the PR for the current branch; if none is found, tell the user a PR is required and do nothing.
---

# PR Messages

Write a concise, accurate message for an existing GitHub pull request
using the `gh` CLI. Base the message only on data `gh` returns. Never
invent PR content.

## Resolve the PR

1. If the user named a PR number (e.g. "PR #123", "123"), use it.
2. Otherwise detect the PR for the current branch:
   - `gh pr view --json number -q .number`
   - if that fails, run `gh pr list --head "$(git branch --show-current)" --json number -q '.[0].number'`
3. If no PR is found, stop. Tell the user: "No PR found for the current branch. Give me a PR number or open a PR first." Do not write a message.

## Gather PR data

Collect everything the message needs with `gh`:

- `gh pr view <number>` -- title, body, state, author, base/head branches
- `gh pr view <number> --json files,commits,labels,reviewDecision,url` -- changed files, commits, labels, review state, URL
- `gh pr diff <number>` -- the actual diff, when a detailed summary is needed

## Follow the repo's release-please conventions

PRs are squash-merged. The PR title becomes the commit on `main` that
release-please reads to compute the next version bump and changelog entry.
Make the title a valid Conventional Commit:

- Type: one of `feat`, `fix`, `chore`, `docs`, `refactor`, `test`, `ci`,
  `build`, `perf`, `revert`. The pr-title-lint workflow enforces this.
- Add `!` after the type or scope for breaking changes.
- Which crate(s) bump is decided by changed paths, not the title's type or
  scope: any change under `crates/<name>` bumps that crate directly, and
  release-please cascades to every crate that depends on it.
- Keep the title short and factual.

## Write the message

Summarize what the PR changes and why, the key files, and anything notable
(breaking changes, dependencies, review state). Match the tone and length
the user asks for; by default, write a short summary suitable for a PR
description or an announcement. If the user asks for a title, produce a
Conventional Commit title per the conventions above. Follow the repo
writing style: no emojis, no em-dashes.

## Rules

- Use `gh` for all PR data; do not guess or fabricate details.
- If `gh` fails (not authenticated, no such PR), report the error and stop.
- Do not create or modify the PR unless the user asks.
