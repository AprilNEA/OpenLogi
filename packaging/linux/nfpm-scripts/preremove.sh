#!/bin/sh
set -eu

# Reload udev rules so the removed uaccess tags take effect immediately.
if command -v udevadm > /dev/null 2>&1; then
    udevadm control --reload-rules
    udevadm trigger --subsystem-match=hidraw
    udevadm trigger --subsystem-match=misc --attr-match=name=uinput 2>/dev/null || true
fi
