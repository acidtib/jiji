#!/usr/bin/env bash
# Appends the actual CHANGELOG.md content of any internal (workspace-path)
# dependency that bumped in this release cycle to a release body.
#
# release-please's cargo-workspace plugin cannot generate a useful
# "workspace dependencies were updated" note for jiji's crates: its
# CargoToml updater only rewrites an inline `{ path = ..., version = ... }`
# dependency declared directly in a crate's own Cargo.toml, and explicitly
# skips anything using Cargo's `dep.workspace = true` shorthand (which is
# how every crate here declares its internal deps) -- confirmed by reading
# release-please's own source. So this script detects bumps itself, by
# diffing `.release-please-manifest.json` (which release-please DOES keep
# accurate) against its state one commit ago, restricted to whatever the
# releasing package (arg 1, a Cargo package name e.g. "jiji-cli") actually
# depends on locally (via `cargo metadata`) -- not every package that
# happened to bump in the same release cycle.
#
# Usage: expand-dependency-changelog.sh <package-name> < release_body > expanded_body
# Must run from the repository root, with git history reaching HEAD~1
# (actions/checkout needs fetch-depth: 0 or >= 2) and cargo available.
set -euo pipefail

package_name="$1"
body="$(cat)"
printf '%s\n' "$body"

deps=$(cargo metadata --no-deps --format-version 1 | \
  jq -r --arg pkg "$package_name" \
    '.packages[] | select(.name == $pkg) | .dependencies[] | select(.path != null) | .name')

previous_manifest=$(git show HEAD~1:.release-please-manifest.json 2>/dev/null || echo '{}')
current_manifest=$(cat .release-please-manifest.json)

sections=""
for dep in $deps; do
  path="crates/$dep"
  old_version=$(printf '%s' "$previous_manifest" | jq -r --arg p "$path" '.[$p] // empty')
  new_version=$(printf '%s' "$current_manifest" | jq -r --arg p "$path" '.[$p] // empty')
  if [ -n "$new_version" ] && [ "$old_version" != "$new_version" ]; then
    changelog="$path/CHANGELOG.md"
    if [ -f "$changelog" ]; then
      section=$(awk '/^## / { n++ } n==1' "$changelog")
      sections+=$(printf '\n<details><summary>%s</summary>\n\n%s\n\n</details>\n' "$dep" "$section")
    fi
  fi
done

if [ -n "$sections" ]; then
  # Trailing newline is required: this output is captured between two
  # delimiter lines in a GITHUB_OUTPUT heredoc block by every caller. Without
  # it, the closing delimiter gets appended to this output's last line
  # instead of starting its own line, and GitHub's parser fails with
  # "Matching delimiter not found" (confirmed live).
  printf '\n### Crate changes in this release\n%s\n' "$sections"
fi
