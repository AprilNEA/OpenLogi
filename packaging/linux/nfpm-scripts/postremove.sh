#!/bin/sh
set -eu

# Reload udev rules and wait for the uaccess revocation to take effect.
# Skipped on OSTree systems, where udev is unreachable from the scriptlet
# sandbox (see postinstall.sh).
if [ ! -e /run/ostree-booted ] && command -v udevadm >/dev/null 2>&1; then
  udevadm control --reload-rules
  udevadm trigger --subsystem-match=hidraw
  udevadm trigger --subsystem-match=misc --attr-match=name=uinput 2>/dev/null || true
  udevadm settle 2>/dev/null || true
fi
