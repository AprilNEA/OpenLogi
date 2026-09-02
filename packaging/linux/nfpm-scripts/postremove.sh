#!/bin/sh
set -eu

# Both package managers call this on upgrade as well as removal, and they spell
# the distinction differently: dpkg passes "remove"/"purge"/"upgrade", rpm passes
# the number of instances left behind (0 on the final removal, >=1 on upgrade).
# Stopping the agent on an upgrade would turn autostart off on every update, so
# only act when the package is actually going away.
removing=no
case "${1:-}" in
  0 | remove | purge) removing=yes ;;
esac

if [ "$removing" = yes ]; then
  # The agent is a *user* service, but this script runs as root under the package
  # manager, so there is no single session to target. Walk the runtime directories
  # of logged-in users instead.
  #
  # `disable --now` rather than `stop`: the unit is Restart=on-failure, and the
  # agent may also have written its own copy to $XDG_CONFIG_HOME/systemd/user/ for
  # launch-at-login. Both carry the same unit name, so one disable stops whichever
  # is active and drops the .wants symlink that would otherwise start it again.
  # Removing the unit file first does not defeat this: systemd matches .wants
  # symlinks by unit name, so a dangling one is still cleaned up here.
  #
  # /run/user covers logged-in and lingering users. A logged-out user without
  # linger keeps a dangling symlink that starts nothing (the unit file went with
  # the package) until the package is reinstalled. That is deliberate, not an
  # oversight: root has no route into a logged-out user's systemd instance, and
  # the standard tooling does not try either — Fedora's %systemd_user_* macros
  # and Debian's deb-systemd-helper both manage global enablement under
  # /etc/systemd/user and never touch per-user config. Do not "fix" this by
  # walking /home and deleting files inside user config directories.
  if command -v systemctl >/dev/null 2>&1; then
    if command -v runuser >/dev/null 2>&1; then
      as_user="runuser -u"
    elif command -v sudo >/dev/null 2>&1; then
      as_user="sudo -u"
    else
      as_user=""
    fi

    if [ -n "$as_user" ]; then
      for runtime in /run/user/*; do
        [ -d "$runtime" ] || continue
        uid=${runtime#/run/user/}
        case $uid in
          '' | *[!0-9]*) continue ;;
        esac
        user=$(id -nu "$uid" 2>/dev/null) || continue
        # shellcheck disable=SC2086 # as_user is a command plus its flag, split on purpose
        $as_user "$user" -- env XDG_RUNTIME_DIR="$runtime" \
          systemctl --user disable --now openlogi-agent.service >/dev/null 2>&1 || true
      done
    fi
  fi

  # An agent the desktop app launched is not under systemd at all — the GUI spawns
  # it from beside its own executable — so the disable above cannot reach it, and
  # it would keep serving IPC to whatever connects next. Match on the executable
  # path so a Flatpak or source-built agent, which the user may still want, is left
  # alone. Our binary is already gone by this point, hence the "(deleted)" form.
  if command -v pgrep >/dev/null 2>&1; then
    for pid in $(pgrep -x openlogi-agent 2>/dev/null || true); do
      exe=$(readlink "/proc/${pid}/exe" 2>/dev/null) || continue
      case "$exe" in
        /usr/bin/openlogi-agent | "/usr/bin/openlogi-agent (deleted)")
          kill "$pid" 2>/dev/null || true
          ;;
      esac
    done
  fi
fi

# Reload udev rules and wait for the uaccess revocation to take effect.
if command -v udevadm >/dev/null 2>&1; then
  udevadm control --reload-rules
  udevadm trigger --subsystem-match=hidraw
  udevadm trigger --subsystem-match=misc --attr-match=name=uinput 2>/dev/null || true
  udevadm settle 2>/dev/null || true
fi
