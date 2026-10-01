#!/bin/sh
set -eu

# /etc/modules-load.d/openlogi.conf loads uinput from the next boot on; load it
# now so the first session after installing works too. Without the module there
# is no uinput device for the rule's uaccess tag to apply to — the node exists
# via static_node= but stays root-owned, so the agent's hook cannot open it and
# button remapping silently does nothing while HID++ keeps working.
if command -v modprobe >/dev/null 2>&1; then
  modprobe uinput || true
fi

# Reload udev rules and wait for the new uaccess tags to be applied.
# udevadm trigger is asynchronous — settle ensures the tags are in place
# before the script exits so the agent can open /dev/hidraw*, /dev/uinput and
# the mouse's /dev/input/event* node immediately, even for a device connected
# before the install.
if command -v udevadm >/dev/null 2>&1; then
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
