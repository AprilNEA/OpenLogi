#!/usr/bin/env bash
# Read-only macOS permission diagnosis for OpenLogi. Safe to hand to a reporter:
# it changes nothing. One step needs root; without it the script prints the
# command to run instead.
set -uo pipefail

app=${OPENLOGI_APP:-/Applications/OpenLogi.app}
# The helper is named after its display name; bundles built before that rename
# spell it without the space. Same identity (org.openlogi.agent) either way.
helper_bundle() {
  for name in "OpenLogi Agent.app" "OpenLogiAgent.app"; do
    [ -d "$1/Contents/Library/LoginItems/$name" ] && {
      printf '%s\n' "$1/Contents/Library/LoginItems/$name"
      return
    }
  done
  printf '%s\n' "$1/Contents/Library/LoginItems/OpenLogi Agent.app"
}
agent=$(helper_bundle "$app")

say() { printf '\n== %s\n' "$1"; }

say "1. Is the agent running, and which binary is it?"
pid=$(pgrep -x openlogi-agent | head -1)
if [ -z "${pid:-}" ]; then
  echo "   no openlogi-agent process — the GUI cannot show devices without it"
else
  count=$(pgrep -x openlogi-agent | wc -l | tr -d ' ')
  if [ "$count" -gt 1 ]; then
    echo "   WARNING: $count agent processes are running — each is its own TCC identity:"
    pgrep -x openlogi-agent | while read -r p; do
      echo "      pid $p: $(ps -o comm= -p "$p")"
    done
    echo "   the rest of this report inspects pid $pid only; quit the others first"
  fi
  running=$(ps -o comm= -p "$pid")
  echo "   $running"
  # The identity that matters is the one actually running. A dev bundle, a
  # second copy, or a build in ~/Downloads is a different identity from the
  # installed app, and a grant to one says nothing about the other.
  case "$running" in
    "$app"/*) ;;
    *.app/Contents/*)
      # `%%` strips at the first ".app/Contents/" -> the outer app bundle;
      # `%` strips at the last one -> the helper bundle it is nested in.
      app=${running%%.app/Contents/*}.app
      agent=${running%.app/Contents/*}.app
      echo "   WARNING: this is not the installed app"
      echo "            grants given to one copy do not apply to another"
      echo "   inspecting the running copy instead: $app"
      ;;
    *)
      echo "   WARNING: running as a bare binary, not from an app bundle."
      echo "            Its ad-hoc identity changes on every rebuild, and TCC"
      echo "            attributes it to the terminal that launched it."
      echo "   inspecting the installed app below for reference: $app"
      ;;
  esac
fi

say "2. Responsible process (must be the agent itself, not the GUI or a terminal)"
if [ -z "${pid:-}" ]; then
  echo "   skipped: agent not running"
elif [ "$(id -u)" -eq 0 ]; then
  launchctl procinfo "$pid" | grep -i responsible || echo "   (no responsible line)"
else
  echo "   needs root; run: sudo launchctl procinfo $pid | grep -i responsible"
fi

say "3. Identities — TCC keys on these, not on the app you see in Finder"
# The overlay is omitted on purpose: it is the one identity that holds no TCC
# grants, so there is nothing to compare.
for target in "$app" "$agent" "$app/Contents/MacOS/openlogi"; do
  [ -e "$target" ] || {
    echo "   missing: $target"
    continue
  }
  printf '   %s\n' "$target"
  codesign -d --verbose=2 "$target" 2>&1 | grep -E '^Identifier=|^TeamIdentifier=|flags=' | sed 's/^/      /'
done

say "4. Signature integrity (a broken signature fails the TCC requirement match)"
for target in "$app" "$agent"; do
  [ -d "$target" ] || continue
  if out=$(codesign --verify --strict "$target" 2>&1); then
    echo "   OK: $target"
  else
    echo "   FAILED: $target"
    printf '      %s\n' "$out"
  fi
done

say "5. Designated requirement recorded against the agent's grant"
[ -d "$agent" ] && codesign -d --requirements - "$agent" 2>&1 | grep '^designated' | sed 's/^/   /'

say "6. The agent's own log"
# The agent ignores a relative XDG_STATE_HOME, as required by the XDG spec.
state_home=${XDG_STATE_HOME:-"$HOME/.local/state"}
case "$state_home" in
  /*) ;;
  *) state_home="$HOME/.local/state" ;;
esac
log_dir="$state_home/openlogi"
# Names are agent.<ISO date>.log, so the lexically last one is the newest.
logs=("$log_dir"/agent.*.log)
latest_log=${logs[${#logs[@]} - 1]}
if [ -f "$latest_log" ]; then
  echo "   $latest_log"
  echo "   attach this file — it holds the classified open errors and any panic"
else
  echo "   none under $log_dir (agent predates the log file, or never ran)"
fi
cat <<TXT

   launchd discards the agent's stderr, so if the file above is missing or
   predates the failure, reproduce it in the foreground:

     OPENLOGI_LOG=debug "$agent/Contents/MacOS/openlogi-agent"

   Caveat: run from a terminal, macOS judges the TERMINAL's Input Monitoring
   permission, not the agent's. A successful open here says nothing about the
   copy launchd runs, and a denial here may only mean the terminal has no
   grant.

   Then classify the first failure you see. The open error names its own
   cause; the identity it speaks about is the one in the caveat above:
     "HID++ candidate interfaces count=0"      -> device not matched; not a permission problem
     "Failed to open device: Input Monitoring is NOT granted ..."
                                               -> grant it to OpenLogi Agent
     "Failed to open device: Input Monitoring is granted ..."
                                               -> another app holds the device (quit Logi
                                                  Options+), or log out and back in
     "opened HID++ channel" then a probe error -> the async-hid write bug is present; this
                                                  run's permission was fine, but the launchd
                                                  copy's grant is still unproven
TXT
