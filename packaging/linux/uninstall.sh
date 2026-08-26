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
sudo rm -f /usr/lib/udev/rules.d/70-openlogi.rules
# The /etc path is the pre-0.9 install location — but it is also the admin
# override directory, so only remove a copy whose effective rules (comments
# and blank lines aside, so header-only edits don't matter) match a body ever
# shipped there; a modified file may be a deliberate policy and stays.
effective_rules() {
  sed -e '/^[[:space:]]*#/d' -e '/^[[:space:]]*$/d' "$1"
}
# The only two bodies ever installed to /etc: the original, and the one after
# the input event-node rules were added (#530). 0.9+ never writes /etc, so
# this set is frozen — it needs no update when the packaged rules change.
SHIPPED_RULES_V1='SUBSYSTEM=="hidraw", ATTRS{idVendor}=="046d", TAG+="uaccess"
SUBSYSTEM=="hidraw", KERNELS=="*:046D:*", TAG+="uaccess"
KERNEL=="uinput", TAG+="uaccess", OPTIONS+="static_node=uinput"'
SHIPPED_RULES_V2="${SHIPPED_RULES_V1}"'
SUBSYSTEM=="input", KERNEL=="event*", ENV{ID_INPUT_MOUSE}=="1", ATTRS{idVendor}=="046d", TAG+="uaccess"
SUBSYSTEM=="input", KERNEL=="event*", ENV{ID_INPUT_MOUSE}=="1", KERNELS=="*:046D:*", TAG+="uaccess"'
ETC_RULES=/etc/udev/rules.d/70-openlogi.rules
if [ -f "$ETC_RULES" ]; then
  # A read failure (say, a root-owned 0600 override) must fall through to the
  # warning, not trip set -e and abort the uninstall half-done.
  ETC_EFFECTIVE="$(effective_rules "$ETC_RULES")" || ETC_EFFECTIVE=
  if [ "$ETC_EFFECTIVE" = "$SHIPPED_RULES_V1" ] ||
    [ "$ETC_EFFECTIVE" = "$SHIPPED_RULES_V2" ]; then
    sudo rm -f "$ETC_RULES"
  else
    echo "Warning: $ETC_RULES does not match any rules OpenLogi ever shipped and" >&2
    echo "was left in place. If it is not a deliberate override, remove it with:" >&2
    echo "  sudo rm $ETC_RULES && sudo udevadm control --reload-rules" >&2
  fi
fi
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
