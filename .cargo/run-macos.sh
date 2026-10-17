#!/usr/bin/env bash
#
# Cargo `runner` for macOS (wired in `.cargo/config.toml`).
#
# Cargo calls this for every binary of every `cargo run`/`test`/`bench`, so the
# passthrough below has to stay cheap — that is why this is still a shell script
# and not something with an interpreter to start. Only `openlogi-desktop` gets
# wrapped, and the wrapping itself lives in `xtask macos dev-bundle`, sharing
# the identity, helper and `Info.plist` tables with what packaging ships.
#
#   OPENLOGI_DEV_BUNDLE=0           run the raw binary, no bundle
#   OPENLOGI_DEV_CODESIGN=0         skip dev codesigning
#   OPENLOGI_DEV_CODESIGN_IDENTITY  pin a signing identity
#   OPENLOGI_DEV_AGENT=0            don't build or embed the agent + overlay
#   OPENLOGI_ALLOW_EXTERNAL_AGENT=1 tolerate an agent outside this checkout
set -euo pipefail

# SIP strips DYLD_* when it launches this interpreter, including the
# DYLD_FALLBACK_LIBRARY_PATH cargo sets so a test binary can find its
# dynamically-linked libstd — which is how a proc-macro crate's tests die with
# "Library not loaded: @rpath/libstd-*.dylib". Rebuild what cargo meant to pass,
# in one `rustc` call and expansions that work on macOS's bash 3.2.
if [ -z "${DYLD_FALLBACK_LIBRARY_PATH:-}" ]; then
  rustc_print="$(rustc --print sysroot --print host-tuple)"
  export DYLD_FALLBACK_LIBRARY_PATH="${rustc_print%%$'\n'*}/lib/rustlib/${rustc_print##*$'\n'}/lib"
fi

bin="$1"
shift

if [ "${bin##*/}" != "openlogi-desktop" ] || [ "${OPENLOGI_DEV_BUNDLE:-1}" = "0" ]; then
  exec "$bin" "$@"
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cargo run -q -p xtask --manifest-path "$ROOT/Cargo.toml" -- macos dev-bundle --binary "$bin"
exec "$ROOT/target/dev/OpenLogi.app/Contents/MacOS/openlogi-desktop" "$@"
