#!/usr/bin/env python3
"""Fold every release-worthy commit since the last tag into the root CHANGELOG.

release-plz only attributes commits to packages it processes. App crates with
`release = false` (gui/agent/agent-core) never get a commit list, so
`changelog_include` cannot surface their work. This script re-reads
`git log` since the latest `v*` tag and merges any missing conventional
commits into the top version section of CHANGELOG.md.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

RELEASE_WORTHY = re.compile(
    r"^(?P<type>feat|fix|perf|security)(?P<breaking>!)?"
    r"(?:\((?P<scope>[^)]+)\))?!?:\s*(?P<summary>.+)$"
)
PR_IN_SUBJECT = re.compile(r"\(#(\d+)\)\s*$")
PR_IN_ENTRY = re.compile(r"\[#(\d+)\]")
PR_LINK_TAIL = re.compile(r"\s*\(\[#\d+\]\([^)]+\)\)\s*$")
SCOPE_PREFIX = re.compile(r"^\*\([^)]+\)\*\s*")
BREAKING_PREFIX = re.compile(r"^\[\*\*breaking\*\*\]\s*")
VERSION_HEADER = re.compile(r"^## \[([^\]]+)\]")

GROUP_FOR_TYPE = {
    "feat": "Added",
    "fix": "Fixed",
    "perf": "Changed",
    "security": "Security",
}

DEFAULT_REPO = "https://github.com/AprilNEA/OpenLogi"


def _summary_text(summary: str, pr: str | None) -> str:
    text = summary.strip()
    if pr:
        text = PR_IN_SUBJECT.sub("", text).rstrip()
    return text


def _message_keys(scope: str | None, summary: str, breaking: bool = False) -> set[str]:
    """Keys used to match a commit against an already-rendered changelog line."""
    text = summary.strip().lower()
    keys = {f"msg:{text}"}
    if scope:
        keys.add(f"msg:*({scope.lower()})* {text}")
        if breaking:
            keys.add(f"msg:*({scope.lower()})* [**breaking**] {text}")
    elif breaking:
        keys.add(f"msg:[**breaking**] {text}")
    return keys


def _keys_from_entry_line(line: str) -> set[str]:
    stripped = line.strip()
    if not stripped.startswith("- "):
        return set()
    body = stripped[2:].strip()
    keys: set[str] = set()
    for pr in PR_IN_ENTRY.findall(body):
        keys.add(f"pr:{pr}")
    # Drop the markdown PR link so bare summary matches commit subjects.
    body = PR_LINK_TAIL.sub("", body).strip()
    keys.add(f"msg:{body.lower()}")
    bare = SCOPE_PREFIX.sub("", body)
    bare = BREAKING_PREFIX.sub("", bare).strip()
    if bare:
        keys.add(f"msg:{bare.lower()}")
    return keys


@dataclass(frozen=True)
class Commit:
    type: str
    scope: str | None
    summary: str
    pr: str | None
    breaking: bool

    def entry_line(self, repo_url: str) -> str:
        text = _summary_text(self.summary, self.pr)
        scope = f"*({self.scope})* " if self.scope else ""
        breaking = "[**breaking**] " if self.breaking else ""
        line = f"- {scope}{breaking}{text}"
        if self.pr:
            line += f" ([#{self.pr}]({repo_url}/pull/{self.pr}))"
        return line

    def fingerprints(self) -> set[str]:
        keys = _message_keys(
            self.scope,
            _summary_text(self.summary, self.pr),
            breaking=self.breaking,
        )
        if self.pr:
            keys.add(f"pr:{self.pr}")
        return keys

    def fingerprint(self) -> str:
        # Stable single key for de-duping within a commit list.
        if self.pr:
            return f"pr:{self.pr}"
        text = _summary_text(self.summary, self.pr).strip().lower()
        if self.scope:
            return f"msg:*({self.scope.lower()})* {text}"
        return f"msg:{text}"

def parse_commit_subject(subject: str) -> Commit | None:
    subject = subject.strip()
    match = RELEASE_WORTHY.match(subject)
    if not match:
        return None
    summary = match.group("summary").strip()
    pr_match = PR_IN_SUBJECT.search(summary)
    pr = pr_match.group(1) if pr_match else None
    return Commit(
        type=match.group("type"),
        scope=match.group("scope"),
        summary=summary,
        pr=pr,
        breaking=bool(match.group("breaking")) or "!" in subject.split(":", 1)[0],
    )


def git_output(*args: str, cwd: Path | None = None) -> str:
    result = subprocess.run(
        ["git", *args],
        cwd=cwd,
        check=True,
        capture_output=True,
        text=True,
    )
    return result.stdout


def latest_version_tag(cwd: Path) -> str:
    tags = [
        line.strip()
        for line in git_output("tag", "--list", "v*", cwd=cwd).splitlines()
        if re.fullmatch(r"v\d+\.\d+\.\d+", line.strip())
    ]
    if not tags:
        raise SystemExit("no vX.Y.Z tags found")
    tags.sort(key=lambda t: [int(p) for p in t[1:].split(".")])
    return tags[-1]


def commits_since(tag: str, cwd: Path) -> list[Commit]:
    raw = git_output("log", f"{tag}..HEAD", "--format=%s", cwd=cwd)
    commits: list[Commit] = []
    seen: set[str] = set()
    for line in raw.splitlines():
        commit = parse_commit_subject(line)
        if commit is None:
            continue
        fp = commit.fingerprint()
        if fp in seen:
            continue
        seen.add(fp)
        commits.append(commit)
    return commits


def existing_fingerprints(section: str) -> set[str]:
    found: set[str] = set()
    for line in section.splitlines():
        found |= _keys_from_entry_line(line)
    return found


def commit_is_present(commit: Commit, present: set[str]) -> bool:
    return bool(commit.fingerprints() & present)


def ensure_section_spacing(body: str) -> str:
    """Blank line after the version header and before the next version header."""
    if not body:
        return "\n"
    if not body.startswith("\n"):
        body = "\n" + body
    # Body must end with a blank line so suffix "## [older]" is separated.
    if not body.endswith("\n"):
        body += "\n"
    if not body.endswith("\n\n"):
        body += "\n"
    return body


def split_top_version_section(changelog: str) -> tuple[str, str, str, str]:
    """Return (prefix, version_header, section_body, suffix)."""
    lines = changelog.splitlines(keepends=True)
    header_idx = None
    for i, line in enumerate(lines):
        if VERSION_HEADER.match(line) and "Unreleased" not in line:
            header_idx = i
            break
    if header_idx is None:
        raise SystemExit("CHANGELOG.md has no version section to augment")

    end_idx = len(lines)
    for j in range(header_idx + 1, len(lines)):
        if VERSION_HEADER.match(lines[j]):
            end_idx = j
            break

    prefix = "".join(lines[:header_idx])
    version_header = lines[header_idx]
    body = "".join(lines[header_idx + 1 : end_idx])
    suffix = "".join(lines[end_idx:])
    return prefix, version_header, body, suffix


def merge_commits_into_section(body: str, commits: list[Commit], repo_url: str) -> str:
    present = existing_fingerprints(body)
    missing = [c for c in commits if not commit_is_present(c, present)]
    if not missing:
        return ensure_section_spacing(body)

    groups: dict[str, list[str]] = {}
    # Preserve existing groups and their lines.
    current_group: str | None = None
    residual: list[str] = []
    for line in body.splitlines():
        if line.startswith("### "):
            current_group = line[4:].strip()
            groups.setdefault(current_group, [])
            continue
        if current_group is None:
            residual.append(line)
            continue
        if line.strip():
            groups[current_group].append(line)

    for commit in missing:
        group = GROUP_FOR_TYPE.get(commit.type, "Other")
        groups.setdefault(group, []).append(commit.entry_line(repo_url))

    # Preferred group order matches Keep a Changelog.
    order = ["Added", "Changed", "Deprecated", "Removed", "Fixed", "Security", "Other"]
    ordered_names = [name for name in order if name in groups] + [
        name for name in groups if name not in order
    ]

    parts: list[str] = []
    if residual and any(r.strip() for r in residual):
        parts.extend(residual)
        if parts and parts[-1] != "":
            parts.append("")

    for name in ordered_names:
        entries = groups[name]
        if not entries:
            continue
        parts.append(f"### {name}")
        parts.append("")
        for entry in entries:
            parts.append(entry.rstrip("\n"))
        parts.append("")

    text = "\n".join(parts)
    return ensure_section_spacing(text)


def augment_changelog(changelog: str, commits: list[Commit], repo_url: str) -> str:
    prefix, version_header, body, suffix = split_top_version_section(changelog)
    new_body = merge_commits_into_section(body, commits, repo_url)
    return f"{prefix}{version_header}{new_body}{suffix}"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--changelog",
        type=Path,
        default=Path("CHANGELOG.md"),
        help="Path to CHANGELOG.md",
    )
    parser.add_argument(
        "--repo-url",
        default=DEFAULT_REPO,
        help="Repo URL used for PR links",
    )
    parser.add_argument(
        "--since-tag",
        default=None,
        help="Override the previous release tag (default: latest vX.Y.Z)",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Print the augmented changelog to stdout instead of writing",
    )
    args = parser.parse_args(argv)

    cwd = Path.cwd()
    tag = args.since_tag or latest_version_tag(cwd)
    commits = commits_since(tag, cwd)
    original = args.changelog.read_text()
    updated = augment_changelog(original, commits, args.repo_url.rstrip("/"))

    if args.dry_run:
        sys.stdout.write(updated)
        return 0

    if updated != original:
        args.changelog.write_text(updated)
        print(
            f"augmented {args.changelog} with {len(commits)} release-worthy "
            f"commit(s) since {tag}",
            file=sys.stderr,
        )
    else:
        print(
            f"no changelog changes needed ({len(commits)} release-worthy "
            f"commit(s) since {tag})",
            file=sys.stderr,
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
