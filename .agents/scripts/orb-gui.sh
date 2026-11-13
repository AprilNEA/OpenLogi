#!/usr/bin/env bash
# Renders the OpenLogi desktop app on the Amp orb Desktop against the mock agent
# and drives it for screenshots. Linux orbs only: .agents/setup installs grim,
# wtype, and wlrctl. See "Headless rendering in a sandbox" in docs/DEVELOPMENT.md.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: .agents/scripts/orb-gui.sh <command> [args]

  start [mock args]   build, then run the mock agent and the GUI as orb services
  restart             rebuild and relaunch the GUI; the mock keeps running
  stop                stop the GUI and the mock agent
  shot [file]         capture the Desktop (default /tmp/orb-gui.png)
  move <x> <y>        move the pointer to absolute Desktop coordinates
  click <x> <y>       move the pointer there and left-click
  run <cmd> [args]    run a command, such as wtype or wlrctl, on the Desktop

start and restart open the component gallery when OPENLOGI_COMPONENT_GALLERY=1.
Mock args such as `--fixture <profile.json>` apply only when the mock starts.
EOF
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# Written by `amp orb desktop ensure`: WAYLAND_DISPLAY, XDG_RUNTIME_DIR, DISPLAY.
# GPUI must use Wayland here; its X11 windows never reach a capture. The mock and
# the GUI both source this file, so they share XDG_RUNTIME_DIR and with it the
# dev IPC socket.
desktop_env="$HOME/.cache/amp/desktop/environment"
app_id=org.openlogi.openlogi

load_desktop_env() {
  if [[ ! -f $desktop_env ]]; then
    echo "The orb Desktop is not running. Run: $0 start" >&2
    exit 1
  fi
  # shellcheck source=/dev/null
  source "$desktop_env"
}

build() {
  (cd "$repo_root" && cargo build -p openlogi-desktop -p openlogi-agent \
    --bin openlogi-desktop --bin openlogi-agent-mock)
}

start_gui() {
  local gallery=""
  if [[ -n ${OPENLOGI_COMPONENT_GALLERY:-} ]]; then
    gallery="OPENLOGI_COMPONENT_GALLERY=$OPENLOGI_COMPONENT_GALLERY"
  fi
  amp orb service start openlogi-gui --cwd "$repo_root" --command \
    ". '$desktop_env' && exec env OPENLOGI_PROFILE=dev OPENLOGI_DEV_AGENT=0 $gallery target/debug/openlogi-desktop"

  if ! timeout 60 wlrctl toplevel waitfor "app_id:$app_id"; then
    echo "No OpenLogi window after 60s; see: amp orb service logs openlogi-gui" >&2
    exit 1
  fi
  # The first launch opens a focused "Check for updates?" window, also titled
  # OpenLogi, beside the main one. Tab focuses its "Not now" button and Return
  # answers it, which marks the prompt seen in the dev config.
  sleep 2
  if (($(wlrctl toplevel list | grep -cx "$app_id: OpenLogi") > 1)); then
    wtype -s 200 -k Tab -k Return
    echo "Answered the first-run update prompt with Not now"
  fi
}

# wlrctl moves the pointer relative to its current position. Pin it to the
# top-left corner first so the coordinates are absolute.
move_pointer() {
  if (($# != 2)); then
    usage >&2
    exit 2
  fi
  wlrctl pointer move -10000 -10000
  wlrctl pointer move "$1" "$2"
}

command="${1:-}"
shift || true
case "$command" in
  start)
    amp orb desktop ensure >/dev/null
    load_desktop_env
    # The Desktop opens a maximized Chrome. Minimize it, rather than kill it, so
    # it stays available to agent-browser.
    wlrctl toplevel minimize app_id:google-chrome || true
    build
    amp orb service start openlogi-mock --cwd "$repo_root" --command \
      ". '$desktop_env' && exec env OPENLOGI_PROFILE=dev target/debug/openlogi-agent-mock ${*@Q}"
    socket="$XDG_RUNTIME_DIR/openlogi-dev/agent.sock"
    seconds=0
    until [[ -S $socket ]]; do
      if ((++seconds > 30)); then
        echo "No mock agent socket at $socket; see: amp orb service logs openlogi-mock" >&2
        exit 1
      fi
      sleep 1
    done
    if wlrctl toplevel find "app_id:$app_id"; then
      echo "OpenLogi is already running; use restart to relaunch it"
    else
      start_gui
    fi
    ;;
  restart)
    load_desktop_env
    build
    amp orb service stop openlogi-gui || true
    start_gui
    ;;
  stop)
    amp orb service stop openlogi-gui || true
    amp orb service stop openlogi-mock || true
    ;;
  shot)
    load_desktop_env
    file="${1:-/tmp/orb-gui.png}"
    # Let GPUI repaint after the last input; an immediate capture can still show
    # the previous hover state.
    sleep 0.5
    grim "$file"
    echo "$file"
    ;;
  move)
    load_desktop_env
    move_pointer "$@"
    ;;
  click)
    load_desktop_env
    move_pointer "$@"
    wlrctl pointer click left
    ;;
  run)
    load_desktop_env
    exec "$@"
    ;;
  *)
    usage >&2
    exit 2
    ;;
esac
