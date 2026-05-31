#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# GPUI's build compiles Metal shaders, which need the real Xcode Metal
# toolchain. A devenv/Nix shell injects a Nix apple-sdk via DEVELOPER_DIR
# that has no `metal`, so force Xcode here — unconditionally, since the
# `:-` default would otherwise defer to that broken value. Override the
# Xcode location with OPENLOGI_DEVELOPER_DIR if it lives elsewhere.
export DEVELOPER_DIR="${OPENLOGI_DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
export SDKROOT="$(/usr/bin/xcrun --sdk macosx --show-sdk-path)"

echo "==> app icon"
"$ROOT/scripts/macos-icns.sh"

if [ "${OPENLOGI_BUNDLE_ASSETS:-0}" = "1" ]; then
  echo "==> device assets: bundling (offline build)"
  cargo run -p openlogi --release -- assets sync
else
  echo "==> device assets: on-demand (not bundled; fetched at first launch)"
  rm -rf "$ROOT/crates/openlogi-gui/assets"
  mkdir -p "$ROOT/crates/openlogi-gui/assets"
fi

echo "==> bundle (.app)"
command -v cargo-bundle >/dev/null 2>&1 || cargo install cargo-bundle --locked
( cd crates/openlogi-gui && cargo bundle --release )
APP="$ROOT/target/release/bundle/osx/OpenLogi.app"
[ -d "$APP" ] || { echo "error: bundle not found at $APP" >&2; exit 1; }

if [ -n "${OPENLOGI_SIGN_IDENTITY:-}" ]; then
  echo "==> codesign ($OPENLOGI_SIGN_IDENTITY)"
  codesign --force --deep --options runtime --timestamp \
           --sign "$OPENLOGI_SIGN_IDENTITY" "$APP"
  codesign --verify --deep --strict "$APP"
else
  echo "==> codesign: skipped (unsigned — set OPENLOGI_SIGN_IDENTITY to sign)"
fi

echo "==> dmg"
command -v create-dmg >/dev/null 2>&1 || {
  echo "error: create-dmg not found — run inside the devenv shell (provided by devenv.nix), or: brew install create-dmg" >&2
  exit 1
}

# Retina background: a multi-rep TIFF (@1x + @2x) prebuilt and hosted on
# assets.openlogi.org. Source art is design/bg/openlogi-dmg-light.svg;
# export it at 760x480 and 1520x960, combine with
#   tiffutil -cathidpicheck light.png light@2x.png -out dmg-background.tiff
# and publish under public/dmg/ in the openlogi-org/assets repo.
BG_TIFF="$ROOT/target/release/dmg-background.tiff"
BG_URL="${OPENLOGI_DMG_BACKGROUND_URL:-https://assets.openlogi.org/dmg/dmg-background.tiff}"
curl -fsSL "$BG_URL" -o "$BG_TIFF" || {
  echo "error: failed to fetch dmg background from $BG_URL" >&2
  exit 1
}

# Geometry below is locked to the painted background (design/bg/*.svg), which
# is 760x480. create-dmg's --window-size is the OUTER window (incl. title bar),
# while the background fills only the content area below it; Finder draws the
# background top-left-aligned without scaling, so a 480-tall window clips the
# bottom of the art (the title bar is 32pt on this macOS). Window height is
# therefore 480 + 32 = 512 so the content area is exactly 480 and nothing is
# cut. Icon coords are content-area-relative, so they stay put.
DMG="$ROOT/target/release/OpenLogi.dmg"
rm -f "$DMG"
create-dmg \
  --volname "OpenLogi" \
  --background "$BG_TIFF" \
  --window-pos 240 120 \
  --window-size 760 512 \
  --icon-size 128 \
  --icon "OpenLogi.app" 212 250 \
  --app-drop-link 548 250 \
  --hide-extension "OpenLogi.app" \
  "$DMG" \
  "$APP"

if [ -n "${OPENLOGI_SIGN_IDENTITY:-}" ]; then
  echo "==> codesign dmg ($OPENLOGI_SIGN_IDENTITY)"
  codesign --force --timestamp --sign "$OPENLOGI_SIGN_IDENTITY" "$DMG"
  codesign --verify --verbose=2 "$DMG"
fi

echo
echo "done → $DMG"
