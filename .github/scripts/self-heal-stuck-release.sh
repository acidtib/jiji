#!/usr/bin/env bash
# Recovers a release-please PR stuck labeled "autorelease: pending" after
# merge: release-please aborts all future releases while one is stuck like
# this. Root cause: an unfixed release-please bug (getBranchComponent vs
# getComponent, upstream #2214) that drops the tag/release for a merged PR
# whenever exactly one package -- always jiji-cli here, since it's the only
# one with include-component-in-tag: false -- was released alone.
#
# Re-derives which tag(s) the stuck merge commit should have produced from
# the manifest diff (not the PR body, which is what release-please itself
# got wrong), creates whatever's missing, then flips the label. Idempotent:
# skips tags that already exist, so reruns and partial prior runs are safe.
#
# Requires: full history checkout (fetch-depth: 0) with a push-capable
# token (default GITHUB_TOKEN can't trigger downstream release workflows),
# and GH_TOKEN set for `gh`.
set -euo pipefail

repo="${GITHUB_REPOSITORY:?GITHUB_REPOSITORY must be set}"

pending_prs=$(gh pr list --repo "$repo" --state merged \
  --label "autorelease: pending" --json number,mergeCommit --limit 20)

pending_count=$(printf '%s' "$pending_prs" | jq 'length')
if [ "$pending_count" -eq 0 ]; then
  echo "No release PRs stuck in autorelease: pending."
  exit 0
fi

tag_for_package() {
  local path="$1" version="$2"
  local component include_component separator
  component=$(jq -r --arg p "$path" \
    '.packages[$p].component // .packages[$p]["package-name"] // empty' \
    release-please-config.json)
  include_component=$(jq -r --arg p "$path" \
    'if (.packages[$p] | has("include-component-in-tag"))
     then .packages[$p]["include-component-in-tag"]
     else true end' \
    release-please-config.json)
  separator=$(jq -r --arg p "$path" \
    '.packages[$p]["tag-separator"] // "-"' \
    release-please-config.json)
  if [ "$include_component" = "true" ]; then
    printf '%s%sv%s' "$component" "$separator" "$version"
  else
    printf 'v%s' "$version"
  fi
}

tag_exists() {
  gh api "repos/$repo/git/ref/tags/$1" >/dev/null 2>&1
}

# Packages whose version actually changed at $sha relative to its parent
# (a manifest lists every package's current version on every commit, so a
# plain key listing would wrongly include every package, not just the ones
# this specific PR released).
changed_packages() {
  local sha="$1"
  local current parent
  current=$(git show "$sha:.release-please-manifest.json")
  parent=$(git show "$sha~1:.release-please-manifest.json" 2>/dev/null || echo '{}')
  jq -n --argjson cur "$current" --argjson par "$parent" \
    '$cur | to_entries[] | select(.value != ($par[.key] // null)) | "\(.key)=\(.value)"' \
    -r
}

for i in $(seq 0 $((pending_count - 1))); do
  pr=$(printf '%s' "$pending_prs" | jq -c ".[$i]")
  number=$(printf '%s' "$pr" | jq -r '.number')
  sha=$(printf '%s' "$pr" | jq -r '.mergeCommit.oid // empty')

  if [ -z "$sha" ]; then
    echo "PR #$number: no merge commit recorded yet, skipping"
    continue
  fi

  echo "PR #$number (merge commit $sha): checking for missing tags/releases"

  fully_healed=1
  while IFS='=' read -r path version; do
    [ -z "$path" ] && continue
    tag=$(tag_for_package "$path" "$version")

    if tag_exists "$tag"; then
      continue
    fi

    echo "  missing: $tag (path=$path, version=$version) -- creating"

    git tag "$tag" "$sha"
    git push origin "$tag"

    dir="${path#crates/}"
    notes=$(git show "$sha:$path/CHANGELOG.md" 2>/dev/null | awk '/^## / { n++ } n==1' || true)
    if [ -z "$notes" ]; then
      notes="Release $tag."
    fi

    gh release create "$tag" \
      --repo "$repo" \
      --target "$sha" \
      --title "$tag" \
      --notes "$notes"

    if ! tag_exists "$tag"; then
      fully_healed=0
    fi
  done < <(changed_packages "$sha")

  if [ "$fully_healed" -eq 1 ]; then
    echo "  all tags present for PR #$number, flipping label to autorelease: tagged"
    gh pr edit "$number" --repo "$repo" \
      --remove-label "autorelease: pending" \
      --add-label "autorelease: tagged"
  else
    echo "  PR #$number still incomplete, leaving it labeled autorelease: pending"
  fi
done
