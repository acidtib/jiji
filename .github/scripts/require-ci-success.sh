#!/usr/bin/env bash
# Gates a release on CI having already passed for the tagged commit, instead of re-running the
# whole suite once per released package. Waits while that run is still in progress (the tag and
# the release commit's own push to main land at almost the same time).
#
# Usage: require-ci-success.sh <tag>
# Requires: GH_TOKEN and GITHUB_REPOSITORY.
set -euo pipefail

tag="$1"
repo="$GITHUB_REPOSITORY"
deadline=$((SECONDS + 75 * 60))
not_found_deadline=$((SECONDS + 10 * 60))

sha=$(gh api "repos/$repo/commits/$tag" --jq .sha)
echo "Checking CI for $tag ($sha)"

while true; do
    runs=$(gh api "repos/$repo/actions/workflows/ci.yml/runs?head_sha=$sha&per_page=20" \
        --jq '[.workflow_runs[] | select(.event == "push" or .event == "workflow_dispatch")
               | {status, conclusion, html_url}]')

    if jq -e 'any(.[]; .conclusion == "success")' <<<"$runs" >/dev/null; then
        echo "CI passed: $(jq -r 'map(select(.conclusion == "success"))[0].html_url' <<<"$runs")"
        exit 0
    fi

    pending=$(jq '[.[] | select(.status != "completed")] | length' <<<"$runs")
    total=$(jq 'length' <<<"$runs")

    if [ "$total" -gt 0 ] && [ "$pending" -eq 0 ]; then
        echo "::error::CI did not pass for $tag ($sha): $(jq -r '.[0].html_url' <<<"$runs"). Fix it on main and release again, or re-run that CI run and then re-run this release."
        exit 1
    fi

    if [ "$total" -eq 0 ] && [ "$SECONDS" -ge "$not_found_deadline" ]; then
        echo "::error::No CI run exists for $tag ($sha). Start one with \`gh workflow run ci.yml --ref $tag\`, then re-run this release once it passes."
        exit 1
    fi

    if [ "$SECONDS" -ge "$deadline" ]; then
        echo "::error::Timed out waiting for CI on $tag ($sha). Re-run this release once CI finishes."
        exit 1
    fi

    echo "Waiting for CI ($pending of $total run(s) in progress)..."
    sleep 30
done
