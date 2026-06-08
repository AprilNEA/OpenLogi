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
        --prefix)   echo "--prefix requires a value" >&2; exit 1 ;;
        *) echo "Unknown argument: $arg" >&2; exit 1 ;;
    esac
done

BINDIR="${PREFIX}/bin"

# ── stop and disable the agent ────────────────────────────────────────────────

if command -v systemctl > /dev/null 2>&1; then
    echo "Disabling and stopping the agent …"
    systemctl --user disable --now openlogi-agent.service 2>/dev/null || true
fi

# ── remove binaries ───────────────────────────────────────────────────────────

echo "Removing binaries …"
sudo rm -f "${BINDIR}/openlogi" "${BINDIR}/openlogi-gui" "${BINDIR}/openlogi-agent"

# ── udev rules ────────────────────────────────────────────────────────────────

echo "Removing udev rules …"
sudo rm -f /etc/udev/rules.d/70-openlogi.rules
if command -v udevadm > /dev/null 2>&1; then
    sudo udevadm control --reload-rules
fi

# ── systemd user unit ─────────────────────────────────────────────────────────

echo "Removing systemd user unit …"
sudo rm -f /usr/lib/systemd/user/openlogi-agent.service

# ── desktop entry + icon ──────────────────────────────────────────────────────

echo "Removing desktop entry and icon …"
sudo rm -f /usr/share/applications/openlogi.desktop
sudo rm -f /usr/share/icons/hicolor/512x512/apps/openlogi.png

if command -v gtk-update-icon-cache > /dev/null 2>&1; then
    sudo gtk-update-icon-cache -qtf /usr/share/icons/hicolor || true
fi
if command -v update-desktop-database > /dev/null 2>&1; then
    sudo update-desktop-database -q /usr/share/applications || true
fi

echo "OpenLogi uninstalled."
