#!/bin/sh
set -eu

# Reload udev rules and wait for the new uaccess tags to be applied.
# udevadm trigger is asynchronous — settle ensures the tags are in place
# before the script exits so the agent can open /dev/hidraw* and the mouse's
# /dev/input/event* node immediately, even for a device connected before the
# install.
#
# Skipped on OSTree systems (Fedora Atomic, Bazzite, Bluefin, ...):
# rpm-ostree runs scriptlets in a bwrap sandbox while composing the next
# deployment, with no access to the host's udev, so `udevadm control` would
# fail and abort the whole `rpm-ostree install`. rpm-ostree exposes
# /run/ostree-booted inside that sandbox for exactly this purpose; the new
# rules are picked up when booting into the new deployment.
if [ ! -e /run/ostree-booted ] && command -v udevadm >/dev/null 2>&1; then
  udevadm control --reload-rules
  udevadm trigger --subsystem-match=hidraw
  udevadm trigger --subsystem-match=input
  udevadm trigger --subsystem-match=misc --attr-match=name=uinput 2>/dev/null || true
  udevadm settle 2>/dev/null || true
fi

# Refresh icon and desktop caches (best-effort).
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -qtf /usr/share/icons/hicolor || true
fi
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database -q /usr/share/applications || true
fi

echo "OpenLogi installed. Enable the background agent for your user with:"
echo "  systemctl --user enable --now openlogi-agent.service"
