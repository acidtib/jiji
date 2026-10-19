#!/usr/bin/env python3
"""Keeps a readable summary comment on the open release-please PR.

The PR body itself must stay untouched: release-please parses it after merge to decide which tags
and releases to create. Version bumps come from the manifest diff (base vs release branch), change
notes from the body's per-package <details> blocks, deduplicated across packages.

Usage: release-pr-summary.py [--dry-run]
Requires: GH_TOKEN and GITHUB_REPOSITORY.
"""

import json
import os
import re
import subprocess
import sys

MARKER = "<!-- jiji-release-summary -->"
RELEASE_BRANCH = "release-please--branches--main"
PUBLISHES = {
    "jiji": "CLI binaries",
    "jiji-agent": "agent binaries",
    "jiji-proxy": "container image",
}
# The only package configured with include-component-in-tag: false; its <summary> has no name.
COMPONENTLESS_PACKAGE = "jiji"

DETAILS = re.compile(r"<details><summary>(.*?)</summary>(.*?)</details>", re.DOTALL)
COMMIT_LINK = re.compile(r"\s*\(\[[0-9a-f]{7,40}\]\([^)]*\)\)")
ISSUE_LINK = re.compile(r"\(\[#(\d+)\]\([^)]*\)\)")


def gh(*args):
    return subprocess.run(["gh", *args], check=True, capture_output=True, text=True).stdout


def file_at(repo, path, ref):
    return gh("api", f"repos/{repo}/contents/{path}?ref={ref}", "-H", "Accept: application/vnd.github.raw")


def parse_notes(body):
    """Returns ({package: [change, ...]}, {section: [change, ...]}) from the release PR body."""
    by_package = {}
    by_section = {}
    for summary, content in DETAILS.findall(body):
        package = summary.split(":", 1)[0].strip() if ":" in summary else COMPONENTLESS_PACKAGE
        changes = by_package.setdefault(package, [])
        section = "Changes"
        for line in content.splitlines():
            line = line.strip()
            if line.startswith("### "):
                section = line[4:].strip()
            elif line.startswith("* "):
                change = ISSUE_LINK.sub(r"(#\1)", COMMIT_LINK.sub("", line[2:])).strip()
                changes.append(change)
                section_changes = by_section.setdefault(section, [])
                if change not in section_changes:
                    section_changes.append(change)
    return by_package, by_section


def render(bumps, by_package, by_section):
    lines = [
        MARKER,
        "## Release summary",
        "",
        "| Package | Version | Publishes | Changes |",
        "|---|---|---|---|",
    ]
    for name, old, new in bumps:
        count = len(by_package.get(name, []))
        changes = f"{count}" if count else "dependency update only"
        lines.append(
            f"| `{name}` | {old} → {new} | {PUBLISHES.get(name, 'nothing (internal crate)')} | {changes} |"
        )
    lines.append("")
    if by_section:
        for section, changes in by_section.items():
            lines.append(f"### {section}")
            lines.extend(f"- {change}" for change in changes)
            lines.append("")
    else:
        lines.extend(["No user-facing changes, only dependency version bumps.", ""])
    lines.append(
        "Merging creates a tag for every package above. Only the packages with something under "
        "\"Publishes\" build and release artifacts; internal crates are tagged so release-please "
        "can track them."
    )
    return "\n".join(lines) + "\n"


def main():
    dry_run = "--dry-run" in sys.argv[1:]
    repo = os.environ["GITHUB_REPOSITORY"]
    prs = json.loads(
        gh("pr", "list", "-R", repo, "--state", "open", "--head", RELEASE_BRANCH,
           "--json", "number,body,baseRefName")
    )
    if not prs:
        print("No open release PR.")
        return
    pr = prs[0]

    config = json.loads(file_at(repo, "release-please-config.json", pr["baseRefName"]))
    names = {path: package["package-name"] for path, package in config["packages"].items()}
    old = json.loads(file_at(repo, ".release-please-manifest.json", pr["baseRefName"]))
    new = json.loads(file_at(repo, ".release-please-manifest.json", RELEASE_BRANCH))
    bumps = [
        (names.get(path, path), old.get(path, "new"), version)
        for path, version in new.items()
        if old.get(path) != version
    ]
    bumps.sort(key=lambda bump: (bump[0] not in PUBLISHES, bump[0]))

    by_package, by_section = parse_notes(pr["body"])
    body = render(bumps, by_package, by_section)
    if dry_run:
        print(body)
        return

    existing = gh(
        "api", "--paginate", f"repos/{repo}/issues/{pr['number']}/comments",
        "--jq", f'.[] | select(.body | startswith("{MARKER}")) | {{id, body}} | @json',
    ).splitlines()
    if existing:
        comment = json.loads(existing[0])
        if comment["body"].strip() == body.strip():
            print(f"Summary on #{pr['number']} is already current.")
            return
        gh("api", "-X", "PATCH", f"repos/{repo}/issues/comments/{comment['id']}", "-f", f"body={body}")
        print(f"Updated summary on #{pr['number']}.")
    else:
        gh("api", f"repos/{repo}/issues/{pr['number']}/comments", "-f", f"body={body}")
        print(f"Posted summary on #{pr['number']}.")


if __name__ == "__main__":
    main()
