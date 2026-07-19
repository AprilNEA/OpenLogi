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
# The dev bundles are codesigned after assembly so LaunchServices/TCC see a
# coherent bundle identity. By default this uses the first Apple Development
# identity in the keychain, falling back to ad-hoc signing; set
# OPENLOGI_DEV_CODESIGN_IDENTITY to choose a specific identity.
#
# Set OPENLOGI_DEV_BUNDLE=0 to skip the wrapper and run the raw binary.
# Set OPENLOGI_DEV_CODESIGN=0 to skip dev codesigning.
# Set OPENLOGI_ALLOW_EXTERNAL_AGENT=1 to let the dev GUI connect to an
# already-running agent outside this checkout (normally a production install).
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
PLIST_SRC="$ROOT/crates/openlogi-gui/bundle/gui-dev/Info.plist"
AGENT_PLIST_SRC="$ROOT/crates/openlogi-gui/bundle/agent-dev/Info.plist"
CODESIGN_ENABLED="${OPENLOGI_DEV_CODESIGN:-1}"

check_external_agent() {
  if [ "${OPENLOGI_ALLOW_EXTERNAL_AGENT:-0}" = "1" ]; then
    return 0
  fi
  local found=0
  local pid path
  while IFS= read -r pid; do
    [ -n "$pid" ] || continue
    path="$(ps -p "$pid" -o comm= 2>/dev/null || true)"
    [ -n "$path" ] || continue
    case "$path" in
      "$APP/Contents/Library/LoginItems/OpenLogi Agent.app/Contents/MacOS/openlogi-agent"|\
      "$ROOT"/target/*/openlogi-agent)
        ;;
      *)
        if [ "$found" = "0" ]; then
          cat >&2 <<EOF
error: an external openlogi-agent is already running.

The dev GUI would connect to that agent instead of the freshly built dev agent,
which makes GUI+Agent testing misleading. Stop the production agent first, e.g.:

  launchctl bootout "gui/$(id -u)/org.openlogi.agent" 2>/dev/null || true
  pkill -x openlogi-agent 2>/dev/null || true

Running external agent(s):
EOF
        fi
        printf '  pid %s: %s\n' "$pid" "$path" >&2
        found=1
        ;;
    esac
  done < <(pgrep -x openlogi-agent 2>/dev/null || true)

  if [ "$found" != "0" ]; then
    cat >&2 <<EOF

If this is intentional, rerun with OPENLOGI_ALLOW_EXTERNAL_AGENT=1.
EOF
    exit 1
  fi
}

mkdir -p "$MACOS" "$RES"
check_external_agent

# App icon — generated from the master PNG on demand. Mirror it
# into the bundle whenever the source is newer (or the bundle copy is missing).
if [ ! -f "$ICON_SRC" ]; then
  cargo run -p xtask --manifest-path "$ROOT/Cargo.toml" -- macos icns
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

# Link/copy the freshly built binary into the bundle. The default unsigned path
# uses a hardlink — instant, no 95 MB copy. The codesigned path must copy:
# codesign mutates the Mach-O signature in place, and a hardlink would rewrite
# Cargo's target/debug artifact behind Cargo's back.
# A hardlink (not a symlink) is required: both NSBundle.mainBundle and Rust's
# current_exe() realpath() the executable, which would resolve a symlink back
# to target/debug/ and break the bundle association. cargo rewrites the binary
# atomically on rebuild (new inode), so relink every run; `ln -f` repoints a
# stale link. Fall back to a copy if the bundle ever lands on another volume.
if [ "$CODESIGN_ENABLED" != "0" ]; then
  cp -f "$bin" "$MACOS/openlogi-gui"
else
  ln -f "$bin" "$MACOS/openlogi-gui" 2>/dev/null || cp -f "$bin" "$MACOS/openlogi-gui"
fi

# Register the dev .app with LaunchServices so the `openlogi://` URL scheme
# works during development. Gate on the *bundled* plist (freshly stamped by the
# copy step above) vs a marker, so a rebuilt bundle re-registers even when the
# source plist is unchanged — and only stamp the marker when lsregister actually
# succeeds, so a failure retries next run instead of latching off. Keep the
# marker outside the .app bundle: codesign treats stray files under Contents as
# bundle resources/subcomponents and refuses to sign an `Info.plist.*` marker.
# Skips the (normally ~10 ms, occasionally multi-second) lsregister cost on the
# steady incremental path.
#
# Both the dev build (here) and the release build register the same openlogi://
# scheme; LaunchServices routes to the last-registered handler. If a release
# install starts winning the scheme during development, re-run this (touch the
# dev plist) or `lsregister -f "$APP"` to put the dev build back in front.
LSREGISTER="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
LSREGISTER_MARKER="$ROOT/target/dev/OpenLogi.app.lsregistered"
if [ -x "$LSREGISTER" ] && [ "$PLIST" -nt "$LSREGISTER_MARKER" ]; then
  if "$LSREGISTER" -R "$APP" 2>/dev/null; then
    touch "$LSREGISTER_MARKER"
  fi
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
  helper="$APP/Contents/Library/LoginItems/OpenLogi Agent.app"
  rm -rf \
    "$APP/Contents/Library/LoginItems/OpenLogiAgent.app" \
    "$APP/Contents/Library/LoginItems/OpenLogi Agent Dev.app"
  mkdir -p "$helper/Contents/MacOS" "$helper/Contents/Resources"
  if [ "$CODESIGN_ENABLED" != "0" ]; then
    cp -f "$agent_dir/openlogi-agent" "$helper/Contents/MacOS/openlogi-agent"
  else
    ln -f "$agent_dir/openlogi-agent" "$helper/Contents/MacOS/openlogi-agent" 2>/dev/null \
      || cp -f "$agent_dir/openlogi-agent" "$helper/Contents/MacOS/openlogi-agent"
  fi
  cp -f "$AGENT_PLIST_SRC" "$helper/Contents/Info.plist"
  # Share the GUI's "e" icon (Info.plist CFBundleIconFile = AppIcon) so the
  # agent isn't a blank entry in the Accessibility list. ICON_SRC was generated
  # / verified above.
  cp -f "$ICON_SRC" "$helper/Contents/Resources/AppIcon.icns"
fi

# Sign after all resources and nested helpers are in place; otherwise macOS can
# cache a stale/invalid bundle identity and TCC may show duplicate-looking rows.
if [ "$CODESIGN_ENABLED" != "0" ]; then
  identity="${OPENLOGI_DEV_CODESIGN_IDENTITY:-}"
  if [ -z "$identity" ]; then
    identity="$(security find-identity -v -p codesigning 2>/dev/null \
      | sed -n 's/.*"\(Apple Development:[^"]*\)".*/\1/p' \
      | head -n 1)"
  fi
  identity="${identity:--}"
  if [ -d "$APP/Contents/Library/LoginItems/OpenLogi Agent.app" ]; then
    codesign --force --sign "$identity" --timestamp=none \
      "$APP/Contents/Library/LoginItems/OpenLogi Agent.app"
  fi
  codesign --force --sign "$identity" --timestamp=none "$APP"
fi

# Register again after the helper is embedded and the bundle is signed, so the
# Accessibility list uses the stamped dev helper name instead of a stale cache.
if [ -x "$LSREGISTER" ]; then
  if "$LSREGISTER" -R "$APP" 2>/dev/null; then
    touch "$LSREGISTER_MARKER"
  fi
fi

exec "$MACOS/openlogi-gui" "$@"
