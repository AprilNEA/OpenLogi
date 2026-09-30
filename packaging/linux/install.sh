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
  /etc/udev/rules.d/70-openlogi.rules
  /usr/lib/systemd/user/openlogi-agent.service  (if systemd is present)
  /usr/share/applications/openlogi.desktop
  /usr/share/icons/hicolor/<size>/apps/openlogi.png  (16 … 1024)
  /usr/share/gnome-shell/extensions/openlogi-frontmost@openlogi.dev/  (GNOME Shell extension)
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
  /etc/udev/rules.d/70-openlogi.rules

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

# ── GNOME Shell companion extension ───────────────────────────────────────────

# Per-app profiles read the focused window's WM_CLASS through this; Mutter
# exposes it nowhere else. Installed, not enabled: GNOME only loads an
# extension the user opted into, and Wayland needs a new session to pick it up.
EXT_SRC="${REPO_ROOT}/crates/openlogi-hook/gnome-shell-extension/openlogi-frontmost@openlogi.dev"
if [ -d "$EXT_SRC" ]; then
  # A native package (.deb/.rpm/.pkg.tar.zst) ships these same two filenames at
  # this same shared path. Overwriting a package-owned copy here would leave
  # its package manager's checksum/record pointing at bytes it didn't put
  # there — mirrors the ownership check uninstall.sh does on removal.
  ext_file_is_package_owned() {
    path="$1"
    if command -v dpkg >/dev/null 2>&1 && dpkg -S "$path" >/dev/null 2>&1; then
      return 0
    fi
    if command -v rpm >/dev/null 2>&1 && rpm -qf "$path" >/dev/null 2>&1; then
      return 0
    fi
    if command -v pacman >/dev/null 2>&1 && pacman -Qo "$path" >/dev/null 2>&1; then
      return 0
    fi
    return 1
  }
  echo "Installing GNOME Shell extension …"
  for file in metadata.json extension.js; do
    target="/usr/share/gnome-shell/extensions/openlogi-frontmost@openlogi.dev/$file"
    if [ -e "$target" ] && ext_file_is_package_owned "$target"; then
      echo "  $file is owned by a distro package — leaving it as installed"
      continue
    fi
    sudo install -Dm644 "$EXT_SRC/$file" "$target"
  done
  echo "  enable it with: gnome-extensions enable openlogi-frontmost@openlogi.dev"
  echo "  (log out and back in first — Wayland cannot reload the shell)"
fi

if command -v update-desktop-database >/dev/null 2>&1; then
  sudo update-desktop-database -q /usr/share/applications || true
fi

echo ""
echo "OpenLogi installed. Run 'openlogi-desktop' to start, or enable the background"
echo "agent with: systemctl --user enable --now openlogi-agent.service"
