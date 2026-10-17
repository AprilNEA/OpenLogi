#!/bin/sh
# OpenLogi Linux uninstall script.
#
# Removes everything install.sh put in place. Requires sudo for system paths.
#
# Usage:
#   ./uninstall.sh [--prefix PREFIX]   (default PREFIX=/usr/local)

set -eu

PREFIX=/usr/local

for arg in "$@"; do
  case "$arg" in
    --prefix=*) PREFIX="${arg#--prefix=}" ;;
    --prefix)
      echo "--prefix requires a value" >&2
      exit 1
      ;;
    *)
      echo "Unknown argument: $arg" >&2
      exit 1
      ;;
  esac
done

BINDIR="${PREFIX}/bin"

# Whether an installed system package (not this script) owns $1 — so removing
# a path both a package and this installer can write to never strands a
# package-managed file still in place (dpkg/rpm, or pacman on an AUR build
# of the .deb).
owned_by_package() {
  if command -v dpkg >/dev/null 2>&1 && dpkg -S "$1" >/dev/null 2>&1; then
    return 0
  fi
  if command -v rpm >/dev/null 2>&1 && rpm -qf "$1" >/dev/null 2>&1; then
    return 0
  fi
  if command -v pacman >/dev/null 2>&1 && pacman -Qo "$1" >/dev/null 2>&1; then
    return 0
  fi
  return 1
}

# ── stop and disable the agent ────────────────────────────────────────────────

# systemctl --user targets the session of whichever user is running this script.
# When invoked via sudo, use SUDO_USER so the command targets the real user's
# session, not root's (which has no agent running).
REAL_USER="${SUDO_USER:-$USER}"
REAL_UID="$(id -u "$REAL_USER")"

if command -v systemctl >/dev/null 2>&1; then
  echo "Disabling and stopping the agent …"
  # Set XDG_RUNTIME_DIR explicitly: sudo -u strips the environment so
  # systemctl --user cannot locate the user's D-Bus socket without it.
  sudo -u "$REAL_USER" XDG_RUNTIME_DIR="/run/user/${REAL_UID}" \
    systemctl --user disable --now openlogi-agent.service 2>/dev/null || true
fi

# ── remove binaries ───────────────────────────────────────────────────────────

echo "Removing binaries …"
sudo rm -f "${BINDIR}/openlogi" "${BINDIR}/openlogi-desktop" \
  "${BINDIR}/openlogi-overlay" "${BINDIR}/openlogi-agent"

# ── udev rules ────────────────────────────────────────────────────────────────

echo "Removing udev rules …"
# Both the current location and the pre-migration one (#1545): an installed
# package still owns either path independently and keeps it on uninstall.
for rule_path in /usr/lib/udev/rules.d/70-openlogi.rules /etc/udev/rules.d/70-openlogi.rules; do
  if [ -e "$rule_path" ]; then
    if owned_by_package "$rule_path"; then
      echo "Keeping $rule_path — owned by an installed package" >&2
    else
      sudo rm -f "$rule_path"
    fi
  fi
done
if command -v udevadm >/dev/null 2>&1; then
  sudo udevadm control --reload-rules
  sudo udevadm trigger --subsystem-match=hidraw
  sudo udevadm trigger --subsystem-match=misc --attr-match=name=uinput 2>/dev/null || true
fi

# ── systemd user unit ─────────────────────────────────────────────────────────

echo "Removing systemd user unit …"
sudo rm -f /usr/lib/systemd/user/openlogi-agent.service

# ── desktop entry + icon ──────────────────────────────────────────────────────

echo "Removing desktop entry and icon …"
sudo rm -f /usr/share/applications/openlogi.desktop
for size in 1024 512 256 128 64 48 32 16; do
  sudo rm -f "/usr/share/icons/hicolor/${size}x${size}/apps/openlogi.png"
done

if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  sudo gtk-update-icon-cache -qtf /usr/share/icons/hicolor || true
fi
if command -v update-desktop-database >/dev/null 2>&1; then
  sudo update-desktop-database -q /usr/share/applications || true
fi

echo "OpenLogi uninstalled."
