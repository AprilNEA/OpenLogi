#!/usr/bin/env bash
# Runs under `op run --environment`, so every variable of the 1Password
# Environment is in this process's environment. Writes the ones named in
# OP_EXPORT_NAMES (action.yml's outputs, parsed by the calling step) to
# GITHUB_OUTPUT, masking each line of each value first. Nothing else may be
# printed: the parent runs `op run --no-masking`.
set -euo pipefail

: "${OP_EXPORT_NAMES:?action.yml passes the output names}"

for name in $OP_EXPORT_NAMES; do
  value="${!name-}"
  [ -n "$value" ] || continue
  while IFS= read -r line || [ -n "$line" ]; do
    [ -n "$line" ] && echo "::add-mask::$line"
  done <<<"$value"
  {
    echo "${name}<<__OP_ENVIRONMENT__"
    printf '%s\n' "$value"
    echo "__OP_ENVIRONMENT__"
  } >>"$GITHUB_OUTPUT"
done
