#!/usr/bin/env bash
# Decides which CI jobs a change needs. Writes key=value lines to
# $GITHUB_OUTPUT (stdout when unset, for local runs):
#
#   run_rust             build, lint, and test the workspace
#   nextest_filter       nextest filterset limiting `test` to changed crates and
#                        their reverse deps; empty means the full suite
#   run_macos            clippy jiji-cli on macOS (a shipped release target)
#   run_docker           docker-in-docker integration suite
#   build_proxy_image    build jiji-proxy from the checkout for the docker suite
#                        (its version is unpublished, or the change touches it)
#   run_deps             lockfile, unused-dependency, and OSV checks
#   run_workflow_checks  actionlint and zizmor
#
# Inputs (env): EVENT_NAME, BASE_REF (pull requests), BEFORE_SHA and
# HEAD_COMMIT_MESSAGE (pushes), HEAD_REF (pull requests), and optionally
# COMPARE_SHA to diff against a fixed commit. Usage outside CI:
#   EVENT_NAME=pull_request BASE_REF=main .github/scripts/ci-changes.sh
set -euo pipefail

out() {
  if [ -n "${GITHUB_OUTPUT:-}" ]; then
    echo "$1=$2" >>"$GITHUB_OUTPUT"
  fi
  echo "$1=$2"
}

emit() {
  out run_rust "$1"
  out nextest_filter "$2"
  out run_macos "$3"
  out run_docker "$4"
  out run_deps "$5"
  out run_workflow_checks "$6"
  out build_proxy_image "$7"
}

proxy_published() {
  local version token
  version="$(sed -n 's/^version = "\(.*\)"/\1/p' crates/jiji-proxy/Cargo.toml | head -1)"
  token="$(curl -fsSL "https://ghcr.io/token?scope=repository:acidtib/jiji-proxy:pull" | jq -r .token)" || return 1
  curl -fsIo /dev/null -H "Authorization: Bearer $token" \
    -H "Accept: application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json" \
    "https://ghcr.io/v2/acidtib/jiji-proxy/manifests/v$version"
}

EVENT_NAME="${EVENT_NAME:?EVENT_NAME is required}"

# A release PR only bumps versions; its merge commit runs the full suite on main.
if [ "${HEAD_REF:-}" = "release-please--branches--main" ]; then
  echo "Release PR, skipping everything"
  emit false "" false false false false false
  exit 0
fi

is_release_push=false
if [ "$EVENT_NAME" = "push" ] && [[ "${HEAD_COMMIT_MESSAGE:-}" == "chore: release"* ]]; then
  is_release_push=true
fi

if [ -n "${COMPARE_SHA:-}" ]; then
  compare="$COMPARE_SHA"
elif [ "$EVENT_NAME" = "pull_request" ]; then
  : "${BASE_REF:?BASE_REF is required for pull requests}"
  git rev-parse --verify --quiet "origin/$BASE_REF" >/dev/null || git fetch --quiet origin "$BASE_REF"
  compare="$(git merge-base "origin/$BASE_REF" HEAD)"
elif [ "$EVENT_NAME" = "push" ] && [ -n "${BEFORE_SHA:-}" ] \
  && [ "$BEFORE_SHA" != "0000000000000000000000000000000000000000" ] \
  && git cat-file -e "$BEFORE_SHA^{commit}" 2>/dev/null; then
  compare="$BEFORE_SHA"
else
  echo "No comparable base ($EVENT_NAME), running everything"
  emit true "" true true true true true
  exit 0
fi

changed="$(git diff --name-only "$compare" HEAD)"
if [ -z "$changed" ]; then
  echo "No changed files"
  emit false "" false false false false false
  exit 0
fi
echo "Changed files since ${compare:0:12}:"
echo "$changed" | awk '{ print "  " $0 }'

has() { echo "$changed" | grep -qP "$1"; }

run_workflow_checks=false
has '^\.github/' && run_workflow_checks=true

run_deps=false
if [ "$EVENT_NAME" = "push" ] || has '(^|/)Cargo\.(toml|lock)$|^osv-scanner\.toml$|^\.github/(workflows/ci\.yml|scripts/ci-)'; then
  run_deps=true
fi

# Anything not known to be non-code counts as code.
non_code='\.md$|^docs/|^LICENSE|^\.gitignore$|^\.release-please-manifest\.json$|^release-please-config\.json$|^\.github/(?!workflows/ci\.yml$|scripts/ci-)'
if ! echo "$changed" | grep -qvP "$non_code"; then
  echo "No code changes"
  emit false "" false false "$run_deps" "$run_workflow_checks" false
  exit 0
fi

# Docker already ran on the PR; on main it only reruns for release commits.
run_docker=true
if [ "$EVENT_NAME" = "push" ] && [ "$is_release_push" = false ]; then
  run_docker=false
fi

build_proxy_image=false
if [ "$run_docker" = true ]; then
  if has '^crates/jiji-proxy/|^Cargo\.(toml|lock)$|^test/docker/'; then
    build_proxy_image=true
  elif ! proxy_published; then
    echo "jiji-proxy's version is not published yet"
    build_proxy_image=true
  fi
fi

full() {
  echo "$1, running the full suite"
  emit true "" true "$run_docker" "$run_deps" "$run_workflow_checks" "$build_proxy_image"
  exit 0
}

[ "$EVENT_NAME" = "push" ] && full "Push"
has '^(Cargo\.(toml|lock)|rust-toolchain(\.toml)?|osv-scanner\.toml)$|^(\.cargo|\.config|lib|test)/|^\.github/(workflows/ci\.yml|scripts/ci-)' \
  && full "Workspace-wide files changed"

metadata="$(cargo metadata --format-version 1 --no-deps)"
dir_to_pkg="$(echo "$metadata" | jq -r '.packages[] | "\(.manifest_path | capture("crates/(?<d>[^/]+)/").d)=\(.name)"')"

packages=""
for dir in $(echo "$changed" | grep -vP '\.md$' | grep -oP '^crates/\K[^/]+' | sort -u); do
  pkg="$(echo "$dir_to_pkg" | grep "^${dir}=" | cut -d= -f2 || true)"
  [ -n "$pkg" ] && packages="$packages $pkg"
done
[ -z "$packages" ] && full "Code changed outside any crate"

# jiji-cli plus every workspace crate it depends on, transitively.
cli_closure="jiji-cli"
frontier="jiji-cli"
while [ -n "$frontier" ]; do
  next=""
  for pkg in $frontier; do
    for dep in $(echo "$metadata" | jq -r --arg p "$pkg" '.packages[] | select(.name == $p) | .dependencies[] | select(.path) | .name'); do
      if ! echo " $cli_closure " | grep -q " $dep "; then
        cli_closure="$cli_closure $dep"
        next="$next $dep"
      fi
    done
  done
  frontier="$next"
done

run_macos=false
for pkg in $packages; do
  echo " $cli_closure " | grep -q " $pkg " && run_macos=true
done

filter="$(for pkg in $packages; do echo "rdeps($pkg)"; done | paste -sd'|' | sed 's/|/ | /g')"
echo "Changed packages:$packages"
emit true "$filter" "$run_macos" "$run_docker" "$run_deps" "$run_workflow_checks" "$build_proxy_image"
