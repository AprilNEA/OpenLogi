#!/usr/bin/env bash
# Reproduce every job in `.github/workflows/ci.yml` that this host can run.
# Commands are copied from that workflow — if you change a `run:` there, change
# the matching function here and the table in `.claude/rules/ci.md`.
#
# Usage:
#   .github/scripts/ci-local.sh              # every host-feasible CI job
#   .github/scripts/ci-local.sh --list       # job → command table
#   .github/scripts/ci-local.sh rustfmt docs # named jobs (CI `name:` or job id)
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"

UNAME="$(uname -s)"
ARCH="$(uname -m)"
DRY_RUN=0
LIST_ONLY=0

passed=0
failed=0
skipped=0
failed_names=""
skipped_names=""

export CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-always}"
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
export RUSTFLAGS="${RUSTFLAGS:--D warnings}"

usage() {
  cat <<'EOF'
Reproduce every job in .github/workflows/ci.yml that this host can run.

Usage:
  .github/scripts/ci-local.sh              every host-feasible CI job
  .github/scripts/ci-local.sh --list       job → command table
  .github/scripts/ci-local.sh --dry-run    print commands, do not run them
  .github/scripts/ci-local.sh rustfmt docs named jobs (CI name: or job id)

Jobs: rustfmt clippy msrv rustdoc tests cargo-deny clippy-windows
Also:  i18n wire   (focused suites that fail the macOS test job)
EOF
}

is_linux() { [ "$UNAME" = Linux ]; }
is_darwin() { [ "$UNAME" = Darwin ]; }
is_windows() {
  case "$UNAME" in
    MINGW*|MSYS*|CYGWIN*|Windows*) return 0 ;;
    *) return 1 ;;
  esac
}

msrv_version() {
  awk -F '"' '/^rust-version = / { print $2; exit }' Cargo.toml
}

need_cargo() {
  if command -v cargo >/dev/null 2>&1; then
    return 0
  fi
  echo "cargo is not on PATH. From the repo root:" >&2
  echo "  direnv exec . $0 $*" >&2
  echo "or enter the devenv shell / install rustup (docs/DEVELOPMENT.md)." >&2
  exit 1
}

note() { printf '    %s\n' "$1"; }

pass_job() {
  passed=$((passed + 1))
  printf 'PASS  %s\n' "$1"
}

fail_job() {
  failed=$((failed + 1))
  failed_names="${failed_names} $1"
  printf 'FAIL  %s\n' "$1"
}

skip_job() {
  skipped=$((skipped + 1))
  skipped_names="${skipped_names} $1"
  printf 'SKIP  %s — %s\n' "$1" "$2"
}

run_cmd() {
  local name="$1"
  shift
  printf '\n==> %s\n' "$name"
  note "$*"
  if [ "$DRY_RUN" -eq 1 ]; then
    pass_job "$name (dry-run)"
    return 0
  fi
  if "$@"; then
    pass_job "$name"
    return 0
  fi
  fail_job "$name"
  return 0
}

job_fmt() {
  run_cmd "rustfmt" cargo fmt --all -- --check
}

job_clippy() {
  printf '\n==> clippy\n'
  if is_linux; then
    note "matches CI job 'clippy' (ubuntu-latest)"
  elif is_windows; then
    note "host Windows clippy; CI's 'clippy' job is ubuntu — also run clippy-windows"
  else
    note "host $UNAME clippy. CI's 'clippy' job is ubuntu-latest and compiles linux cfg — this is not that job"
  fi
  note "cargo clippy --workspace --all-targets -- -D warnings"
  if [ "$DRY_RUN" -eq 1 ]; then
    pass_job "clippy (dry-run)"
    return 0
  fi
  if cargo clippy --workspace --all-targets -- -D warnings; then
    pass_job "clippy"
  else
    fail_job "clippy"
  fi
}

job_msrv() {
  local msrv
  msrv="$(msrv_version)"
  if [ -z "$msrv" ]; then
    skip_job "MSRV" "could not read rust-version from Cargo.toml"
    return 0
  fi
  if is_windows; then
    skip_job "MSRV" "CI's msrv matrix is macos-latest + ubuntu-latest, not Windows"
    return 0
  fi
  printf '\n==> MSRV (cargo check, %s, rustc %s)\n' "$UNAME" "$msrv"
  note "CI sets RUSTUP_TOOLCHAIN=$msrv because rust-toolchain.toml pins stable"
  if [ "$DRY_RUN" -eq 1 ]; then
    pass_job "MSRV (dry-run)"
    return 0
  fi
  if command -v rustup >/dev/null 2>&1 && RUSTUP_TOOLCHAIN="$msrv" rustc -vV >/dev/null 2>&1; then
    if RUSTUP_TOOLCHAIN="$msrv" cargo check --workspace --all-targets; then
      pass_job "MSRV"
    else
      fail_job "MSRV"
    fi
    return 0
  fi
  if rustc -vV 2>/dev/null | grep -q "^release: ${msrv}"; then
    note "RUSTUP_TOOLCHAIN=$msrv unavailable; rustc is already $msrv"
    if cargo check --workspace --all-targets; then
      pass_job "MSRV"
    else
      fail_job "MSRV"
    fi
    return 0
  fi
  skip_job "MSRV" "install the floor: rustup toolchain install ${msrv} (then rerun). rust-toolchain.toml pins stable, so a floating toolchain is not this job"
}

job_docs() {
  run_cmd "rustdoc (non-GUI crates)" env RUSTDOCFLAGS="-D warnings" \
    cargo doc --workspace --no-deps --document-private-items \
    --exclude openlogi-ui \
    --exclude openlogi-desktop \
    --exclude openlogi-overlay \
    --exclude openlogi-agent
}

job_test_linux() {
  if ! is_linux; then
    skip_job "tests (linux)" "needs Linux; this host is $UNAME/$ARCH. Running macOS tests is not this job"
    return 0
  fi
  run_cmd "tests (linux)" cargo test --workspace --exclude openlogi-desktop
}

job_test_macos() {
  if ! is_darwin; then
    skip_job "tests (macos)" "needs macOS; this host is $UNAME/$ARCH"
    return 0
  fi
  printf '\n==> tests (macos, %s)\n' "$ARCH"
  note "CI also has a macos-15-intel x86_64 matrix leg — this host only covers $ARCH"
  note "cargo test --workspace --all-targets"
  if [ "$DRY_RUN" -eq 1 ]; then
    pass_job "tests (macos, $ARCH) (dry-run)"
    return 0
  fi
  if cargo test --workspace --all-targets; then
    pass_job "tests (macos, $ARCH)"
  else
    fail_job "tests (macos, $ARCH)"
  fi
}

job_tests() {
  if is_linux; then
    job_test_linux
  elif is_darwin; then
    job_test_macos
  else
    skip_job "tests" "CI has no Windows test job (clippy-windows only). To run tests anyway: cargo test --workspace --all-targets"
  fi
}

job_deny() {
  printf '\n==> cargo-deny\n'
  note "cargo deny --all-features --manifest-path crates/openlogi/Cargo.toml check"
  if [ "$DRY_RUN" -eq 1 ]; then
    pass_job "cargo-deny (dry-run)"
    return 0
  fi
  if command -v cargo >/dev/null 2>&1 && cargo deny --help >/dev/null 2>&1; then
    if cargo deny --all-features --manifest-path crates/openlogi/Cargo.toml check; then
      pass_job "cargo-deny"
    else
      fail_job "cargo-deny"
    fi
    return 0
  fi
  if command -v cargo-deny >/dev/null 2>&1; then
    if cargo-deny --all-features --manifest-path crates/openlogi/Cargo.toml check; then
      pass_job "cargo-deny"
    else
      fail_job "cargo-deny"
    fi
    return 0
  fi
  if command -v nix >/dev/null 2>&1; then
    if nix run nixpkgs#cargo-deny -- --all-features --manifest-path crates/openlogi/Cargo.toml check; then
      pass_job "cargo-deny"
    else
      fail_job "cargo-deny"
    fi
    return 0
  fi
  skip_job "cargo-deny" "install cargo-deny (cargo install cargo-deny --locked) or nix"
}

job_clippy_windows() {
  if is_windows; then
    run_cmd "clippy (windows)" cargo clippy --workspace --all-targets -- -D warnings
    return 0
  fi
  printf '\n==> clippy (windows) [proxy]\n'
  note "CI runs the whole workspace on windows-latest. This is devenv's ring-free cross lint, not that job"
  note "keep the -p list in sync with devenv.nix openlogi:check-windows"
  if [ "$DRY_RUN" -eq 1 ]; then
    pass_job "clippy (windows) proxy (dry-run)"
    return 0
  fi
  gnu_std="$(rustc --print sysroot)/lib/rustlib/x86_64-pc-windows-gnu"
  if [ ! -d "$gnu_std" ]; then
    skip_job "clippy (windows) proxy" "missing x86_64-pc-windows-gnu std (devenv, or: rustup target add x86_64-pc-windows-gnu)"
    return 0
  fi
  # cargo-clippy, not cargo clippy: rustup's cargo-clippy on PATH shadows devenv's.
  if command -v cargo-clippy >/dev/null 2>&1; then
    if cargo-clippy clippy --target x86_64-pc-windows-gnu \
      -p openlogi-core -p openlogi-hidpp -p openlogi-hid -p openlogi-hook \
      -p openlogi-inject -p openlogi-camera \
      -p openlogi-agent -p openlogi-agent-core \
      --all-targets -- -D warnings; then
      pass_job "clippy (windows) proxy"
    else
      fail_job "clippy (windows) proxy"
    fi
    return 0
  fi
  if cargo clippy --target x86_64-pc-windows-gnu \
    -p openlogi-core -p openlogi-hidpp -p openlogi-hid -p openlogi-hook \
    -p openlogi-inject -p openlogi-camera \
    -p openlogi-agent -p openlogi-agent-core \
    --all-targets -- -D warnings; then
    pass_job "clippy (windows) proxy"
  else
    fail_job "clippy (windows) proxy"
  fi
}

job_i18n() {
  run_cmd "i18n" cargo test -p openlogi-desktop i18n
}

job_wire() {
  run_cmd "wire_format" cargo test -p openlogi-ipc --test wire_format
}

print_list() {
  cat <<'EOF'
CI job (ci.yml)              Local command                                      This host
---------------------------  -------------------------------------------------  ---------
rustfmt                      cargo fmt --all -- --check                         any
clippy                       cargo clippy --workspace --all-targets -- -D warnings
                             CI runs this on ubuntu-latest (linux cfg). Host
                             clippy on macOS/Windows is a different compilation.
MSRV (cargo check, <os>)     RUSTUP_TOOLCHAIN=<rust-version> \
                               cargo check --workspace --all-targets
                             rust-version is in the root Cargo.toml. CI sets
                             RUSTUP_TOOLCHAIN because rust-toolchain.toml is
                             `stable` and would otherwise silently check stable.
rustdoc (non-GUI crates)     RUSTDOCFLAGS="-D warnings" cargo doc --workspace \
                               --no-deps --document-private-items \
                               --exclude openlogi-ui --exclude openlogi-desktop \
                               --exclude openlogi-overlay --exclude openlogi-agent
tests (linux)                cargo test --workspace --exclude openlogi-desktop  Linux
tests (macos, <arch>)        cargo test --workspace --all-targets               macOS
                             CI matrix: arm64 (macos-latest) and x86_64
                             (macos-15-intel). Linux excludes openlogi-desktop,
                             so i18n tests do not run on Linux CI.
cargo-deny                   cargo deny --all-features \
                               --manifest-path crates/openlogi/Cargo.toml check
clippy (windows)             cargo clippy --workspace --all-targets -- -D warnings
                             (windows-latest). Elsewhere: the check-windows
                             proxy in devenv.nix — not the full workspace.

Env CI always sets: CARGO_TERM_COLOR=always CARGO_INCREMENTAL=0 RUSTFLAGS=-D warnings

Focused suites (not their own CI jobs; they fail tests (macos)):
  i18n   cargo test -p openlogi-desktop i18n
  wire   cargo test -p openlogi-ipc --test wire_format

Other PR workflows (not in the default run):
  Nix CI      nix fmt -- --check flake.nix devenv.nix packaging/linux/package.nix \
                packaging/linux/nixos-module.nix
              nix flake check --all-systems --no-build --show-trace
  devenv CI   nix fmt -- --check devenv.nix
              devenv --no-tui shell -- true
  Build       unsigned installers; only when touching xtask/packaging

Full map: .claude/rules/ci.md
EOF
}

run_named() {
  case "$1" in
    rustfmt|fmt) job_fmt ;;
    clippy) job_clippy ;;
    msrv|MSRV|"MSRV (cargo check"*) job_msrv ;;
    rustdoc|docs|"rustdoc (non-GUI crates)") job_docs ;;
    tests) job_tests ;;
    test-linux|"tests (linux)") job_test_linux ;;
    test-macos|"tests (macos"*) job_test_macos ;;
    cargo-deny|deny) job_deny ;;
    clippy-windows|"clippy (windows)") job_clippy_windows ;;
    i18n) job_i18n ;;
    wire|wire_format) job_wire ;;
    -h|--help|help) usage; exit 0 ;;
    --list|list) print_list; exit 0 ;;
    --dry-run) DRY_RUN=1 ;;
    *)
      echo "unknown job: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
}

run_default() {
  job_fmt
  job_clippy
  job_msrv
  job_docs
  job_tests
  job_deny
  job_clippy_windows
}

summarize() {
  printf '\n---- %s passed, %s failed, %s skipped ----\n' "$passed" "$failed" "$skipped"
  if [ -n "$skipped_names" ]; then
    printf 'Skipped:%s\n' "$skipped_names"
    printf 'A skipped job is not a pass. Name it as not run in the PR Testing section.\n'
  fi
  if [ "$failed" -ne 0 ]; then
    printf 'Failed:%s\n' "$failed_names"
    return 1
  fi
  return 0
}

# --- main ---

args=()
for arg in "$@"; do
  case "$arg" in
    -h|--help|help) usage; exit 0 ;;
    --list|list) LIST_ONLY=1 ;;
    --dry-run) DRY_RUN=1 ;;
    *) args+=("$arg") ;;
  esac
done

if [ "$LIST_ONLY" -eq 1 ]; then
  print_list
  exit 0
fi

if [ "$DRY_RUN" -eq 0 ]; then
  need_cargo "$@"
fi

if [ "${#args[@]}" -eq 0 ]; then
  run_default
else
  for job in "${args[@]}"; do
    run_named "$job"
  done
fi

summarize
