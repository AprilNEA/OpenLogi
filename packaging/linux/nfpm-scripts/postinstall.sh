#!/bin/sh
set -eu

# Reload udev rules and wait for the new uaccess tags to be applied.
# udevadm trigger is asynchronous — settle ensures the tags are in place
# before the script exits so the agent can open /dev/hidraw* immediately.
if command -v udevadm > /dev/null 2>&1; then
    udevadm control --reload-rules
    udevadm trigger --subsystem-match=hidraw
    udevadm trigger --subsystem-match=misc --attr-match=name=uinput 2>/dev/null || true
    udevadm settle 2>/dev/null || true
fi

# Refresh icon and desktop caches (best-effort).
if command -v gtk-update-icon-cache > /dev/null 2>&1; then
    gtk-update-icon-cache -qtf /usr/share/icons/hicolor || true
fi
if command -v update-desktop-database > /dev/null 2>&1; then
    update-desktop-database -q /usr/share/applications || true
fi

echo "OpenLogi installed. Enable the background agent for your user with:"
echo "  systemctl --user enable --now openlogi-agent.service"
