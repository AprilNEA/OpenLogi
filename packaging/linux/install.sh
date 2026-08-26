#!/bin/sh
# OpenLogi Linux install script.
#
# Installs the four OpenLogi executables plus udev rules, the systemd user-unit
# template, the .desktop launcher, and the app icon. Requires sudo for the
# system-wide paths.
#
# Usage:
#   ./install.sh [--prefix PREFIX]   (default PREFIX=/usr/local)
#   ./install.sh --help
#
# On systemd systems the udev rules are reloaded automatically. The agent
# must be enabled per-user:
#   systemctl --user enable --now openlogi-agent.service

set -eu

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PREFIX=/usr/local
BINDIR=
PRINT_HELP=0

for arg in "$@"; do
  case "$arg" in
    --prefix=*) PREFIX="${arg#--prefix=}" ;;
    --prefix)
      echo "--prefix requires a value, e.g. --prefix=/usr" >&2
      exit 1
      ;;
    --help | -h) PRINT_HELP=1 ;;
    *)
      echo "Unknown argument: $arg" >&2
      exit 1
      ;;
  esac
done

if [ "$PRINT_HELP" -eq 1 ]; then
  cat <<EOF
Usage: $0 [--prefix PREFIX]

Options:
  --prefix PREFIX   Install binaries under PREFIX/bin (default: /usr/local)
  --help            Show this help

The script installs:
  PREFIX/bin/openlogi
  PREFIX/bin/openlogi-desktop
  PREFIX/bin/openlogi-overlay
  PREFIX/bin/openlogi-agent
  /usr/lib/udev/rules.d/70-openlogi.rules
  /usr/lib/systemd/user/openlogi-agent.service  (if systemd is present)
  /usr/share/applications/openlogi.desktop
  /usr/share/icons/hicolor/<size>/apps/openlogi.png  (16 … 1024)
EOF
  exit 0
fi

BINDIR="${PREFIX}/bin"

# ── locate build output ────────────────────────────────────────────────────────

# Prefer a release build next to the script (typical: run from the repo root
# after `cargo build --release`).
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BUILD_DIR="${REPO_ROOT}/target/release"

for bin in openlogi openlogi-desktop openlogi-overlay openlogi-agent; do
  if [ ! -x "${BUILD_DIR}/${bin}" ]; then
    echo "Error: ${BUILD_DIR}/${bin} not found." >&2
    echo "Build first: cargo build --release -p openlogi -p openlogi-desktop -p openlogi-overlay -p openlogi-agent" >&2
    exit 1
  fi
done

# ── install binaries ───────────────────────────────────────────────────────────

echo "Installing binaries to ${BINDIR} …"
sudo install -Dm755 "${BUILD_DIR}/openlogi" "${BINDIR}/openlogi"
sudo install -Dm755 "${BUILD_DIR}/openlogi-desktop" "${BINDIR}/openlogi-desktop"
sudo install -Dm755 "${BUILD_DIR}/openlogi-overlay" "${BINDIR}/openlogi-overlay"
sudo install -Dm755 "${BUILD_DIR}/openlogi-agent" "${BINDIR}/openlogi-agent"

# ── udev rules ────────────────────────────────────────────────────────────────

echo "Installing udev rules …"
sudo install -Dm644 "${SCRIPT_DIR}/udev/70-openlogi.rules" \
  /usr/lib/udev/rules.d/70-openlogi.rules
# A same-named file in the admin directory shadows the vendor file. Remove it
# only when its effective rules (comments and blank lines aside, so a pre-0.9
# copy still matches after header-only edits here) equal the packaged ones —
# anything else may be a deliberate admin override, which /etc/udev/rules.d
# exists for: leave it and say so.
effective_rules() {
  sed -e '/^[[:space:]]*#/d' -e '/^[[:space:]]*$/d' "$1"
}
ETC_RULES=/etc/udev/rules.d/70-openlogi.rules
if [ -f "$ETC_RULES" ]; then
  if [ "$(effective_rules "$ETC_RULES")" = \
    "$(effective_rules "${SCRIPT_DIR}/udev/70-openlogi.rules")" ]; then
    sudo rm -f "$ETC_RULES"
  else
    echo "Warning: $ETC_RULES differs from the packaged rules and overrides the" >&2
    echo "vendor file just installed. If it is only a leftover from an old install:" >&2
    echo "  sudo rm $ETC_RULES && sudo udevadm control --reload-rules" >&2
  fi
fi

if command -v udevadm >/dev/null 2>&1; then
  echo "Reloading udev rules …"
  sudo udevadm control --reload-rules
  sudo udevadm trigger --subsystem-match=hidraw
  sudo udevadm trigger --subsystem-match=input
  sudo udevadm trigger --subsystem-match=misc --attr-match=name=uinput 2>/dev/null || true
fi

# ── systemd user unit ─────────────────────────────────────────────────────────

SYSTEMD_UNIT_DIR=/usr/lib/systemd/user
if [ -d "$SYSTEMD_UNIT_DIR" ] || command -v systemctl >/dev/null 2>&1; then
  echo "Installing systemd user unit …"
  # Rewrite the packaged /usr/bin path to match the requested install prefix.
  # Escape the replacement so sed metacharacters (& \ |) in the path are literal.
  ESCAPED_BINDIR="$(printf '%s\n' "${BINDIR}" | sed 's|[&\\|]|\\&|g')"
  sed "s|^ExecStart=/usr/bin/openlogi-agent$|ExecStart=${ESCAPED_BINDIR}/openlogi-agent|" \
    "${SCRIPT_DIR}/systemd/openlogi-agent.service" |
    sudo tee "${SYSTEMD_UNIT_DIR}/openlogi-agent.service" >/dev/null
  # Best-effort daemon-reload for the invoking user so a reinstall picks up
  # the updated unit without requiring a manual reload.
  INSTALL_USER="${SUDO_USER:-$USER}"
  sudo -u "$INSTALL_USER" \
    XDG_RUNTIME_DIR="/run/user/$(id -u "$INSTALL_USER")" \
    systemctl --user daemon-reload 2>/dev/null || true
  echo "Enable the agent for your user with:"
  echo "  systemctl --user enable --now openlogi-agent.service"
fi

# ── desktop entry ─────────────────────────────────────────────────────────────

echo "Installing desktop entry …"
sudo install -Dm644 "${SCRIPT_DIR}/desktop/openlogi.desktop" \
  /usr/share/applications/openlogi.desktop

# ── icon ──────────────────────────────────────────────────────────────────────

# Every standard indexed hicolor size, not only the 1024 master: a stock
# `hicolor/index.theme` stops at 512x512, so a launcher that resolves icons
# through the theme index shows nothing when only `1024x1024/apps` exists.
ICON_SRC="${REPO_ROOT}/design/icon/openlogi.png"
if [ -f "$ICON_SRC" ]; then
  echo "Installing icon …"
  sudo install -Dm644 "$ICON_SRC" \
    /usr/share/icons/hicolor/1024x1024/apps/openlogi.png
  for size in 512 256 128 64 48 32 16; do
    sized="${REPO_ROOT}/design/icon/openlogi-${size}.png"
    [ -f "$sized" ] || continue
    sudo install -Dm644 "$sized" \
      "/usr/share/icons/hicolor/${size}x${size}/apps/openlogi.png"
  done
  if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    sudo gtk-update-icon-cache -qtf /usr/share/icons/hicolor || true
  fi
fi

if command -v update-desktop-database >/dev/null 2>&1; then
  sudo update-desktop-database -q /usr/share/applications || true
fi

echo ""
echo "OpenLogi installed. Run 'openlogi-desktop' to start, or enable the background"
echo "agent with: systemctl --user enable --now openlogi-agent.service"
