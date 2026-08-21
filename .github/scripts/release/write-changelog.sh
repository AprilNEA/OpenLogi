#!/usr/bin/env bash
# Write the next workspace version section into CHANGELOG.md with git-cliff.
# Whole-repo conventional commits since the previous v* tag (cliff.toml).
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"

version="$(
  python3 - <<'PY'
import pathlib, re, sys
text = pathlib.Path("Cargo.toml").read_text()
m = re.search(r'(?ms)^\[workspace\.package\].*?^version\s*=\s*"([^"]+)"', text)
if not m:
    sys.exit("workspace.package version not found in Cargo.toml")
print(m.group(1))
PY
)"
tag="v${version}"

last_tag="$(
  git tag --list 'v*' |
    grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' |
    sort -V |
    tail -n1
)"
if [[ -z "${last_tag}" ]]; then
  echo "error: no previous vX.Y.Z tag" >&2
  exit 1
fi
if [[ "${last_tag}" == "${tag}" ]]; then
  echo "error: workspace version ${version} is already tagged as ${tag}" >&2
  exit 1
fi

# Drop a stale section for this version (idempotent re-runs / release-pr updates).
if grep -qE "^## \[${version}\]" CHANGELOG.md; then
  python3 - "${version}" <<'PY'
from pathlib import Path
import re
import sys

version = sys.argv[1]
text = Path("CHANGELOG.md").read_text()
pattern = re.compile(
    rf"(?ms)^## \[{re.escape(version)}\].*?(?=^## \[|\Z)"
)
Path("CHANGELOG.md").write_text(pattern.sub("", text, count=1))
PY
fi

git cliff "${last_tag}.." \
  --config cliff.toml \
  --tag "${tag}" \
  --prepend CHANGELOG.md

echo "wrote ${tag} changelog from ${last_tag}..HEAD" >&2
