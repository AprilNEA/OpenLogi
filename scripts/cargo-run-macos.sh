#!/usr/bin/env bash
#
# Cargo `runner` for macOS — wired in `.cargo/config.toml`.
#
# Cargo hands this script the freshly built binary as $1 for every
# `cargo run` / `cargo test` / `cargo bench` on macOS. For everything except
# the desktop binary it's a transparent passthrough (`exec "$@"`).
#
# For `openlogi-gui` it launches the build from inside a throwaway
# `OpenLogi.app` so macOS shows the real app name (the bold menu-bar title)
# and the Dock icon during development. Both are read from the bundle's
# `Info.plist` / `Resources` — a bare `target/debug/openlogi-gui` has neither,
# so macOS falls back to the executable name and a generic icon.
#
# Set OPENLOGI_DEV_BUNDLE=0 to skip the wrapper and run the raw binary.
set -euo pipefail

bin="$1"
shift

if [ "${bin##*/}" != "openlogi-gui" ] || [ "${OPENLOGI_DEV_BUNDLE:-1}" = "0" ]; then
  exec "$bin" "$@"
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP="$ROOT/target/dev/OpenLogi.app"
MACOS="$APP/Contents/MacOS"
RES="$APP/Contents/Resources"
ICON_SRC="$ROOT/crates/openlogi-gui/icon/AppIcon.icns"
PLIST_SRC="$ROOT/crates/openlogi-gui/dev/Info.plist"

mkdir -p "$MACOS" "$RES"

# App icon — generated from the master PNG on demand. Mirror it
# into the bundle whenever the source is newer (or the bundle copy is missing).
if [ ! -f "$ICON_SRC" ]; then
  cargo run -p xtask --manifest-path "$ROOT/Cargo.toml" -- macos-icns
fi
if [ "$ICON_SRC" -nt "$RES/AppIcon.icns" ]; then
  cp -f "$ICON_SRC" "$RES/AppIcon.icns"
fi

# Info.plist — minimal, dev-only. A distinct `.dev` identifier keeps this
# target artifact from registering as the production app in LaunchServices.
PLIST="$APP/Contents/Info.plist"
if [ "$PLIST_SRC" -nt "$PLIST" ]; then
  cp -f "$PLIST_SRC" "$PLIST"
fi

# Hardlink the freshly built binary into the bundle — instant, no 95 MB copy.
# A hardlink (not a symlink) is required: both NSBundle.mainBundle and Rust's
# current_exe() realpath() the executable, which would resolve a symlink back
# to target/debug/ and break the bundle association. cargo rewrites the binary
# atomically on rebuild (new inode), so relink every run; `ln -f` repoints a
# stale link. Fall back to a copy if the bundle ever lands on another volume.
ln -f "$bin" "$MACOS/openlogi-gui" 2>/dev/null || cp -f "$bin" "$MACOS/openlogi-gui"

# Register the dev .app with LaunchServices so the `openlogi://` URL scheme
# works during development. Only re-register when the Info.plist was updated
# this run — avoids the (normally ~10 ms but occasionally multi-second)
# lsregister cost on every incremental rebuild.
LSREGISTER="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
if [ -x "$LSREGISTER" ] && [ "$PLIST_SRC" -nt "$PLIST.lsregistered" ]; then
  "$LSREGISTER" -R "$APP" 2>/dev/null || true
  touch "$PLIST.lsregistered"
fi

# Embed the headless agent so the GUI can auto-spawn it in dev. The GUI's IPC
# client (ipc_client::agent_binary_path) looks for the agent as the embedded
# login-item helper beside the GUI executable — exactly the production layout
# xtask's embed_agent_helper assembles. `cargo run -p openlogi-gui` builds only
# the GUI, so build the agent in the matching profile and mirror that layout
# here. Cheap after the first build (an incremental no-op); set
# OPENLOGI_DEV_AGENT=0 to run the GUI against a separately launched agent.
if [ "${OPENLOGI_DEV_AGENT:-1}" != "0" ]; then
  agent_dir="$(dirname "$bin")" # target/debug or target/release
  if [ "${agent_dir##*/}" = "release" ]; then
    cargo build -p openlogi-agent --release --manifest-path "$ROOT/Cargo.toml"
  else
    cargo build -p openlogi-agent --manifest-path "$ROOT/Cargo.toml"
  fi
  helper="$APP/Contents/Library/LoginItems/OpenLogiAgent.app"
  mkdir -p "$helper/Contents/MacOS"
  ln -f "$agent_dir/openlogi-agent" "$helper/Contents/MacOS/openlogi-agent" 2>/dev/null \
    || cp -f "$agent_dir/openlogi-agent" "$helper/Contents/MacOS/openlogi-agent"
  cp -f "$ROOT/crates/openlogi-agent/macos/Info.plist" "$helper/Contents/Info.plist"
fi

exec "$MACOS/openlogi-gui" "$@"
