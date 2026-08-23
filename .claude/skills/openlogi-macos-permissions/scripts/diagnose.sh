#!/usr/bin/env bash
# Read-only macOS permission diagnosis for OpenLogi. Safe to hand to a reporter:
# it changes nothing. `sudo` is used for one optional step and skipped without it.
set -uo pipefail

app=${OPENLOGI_APP:-/Applications/OpenLogi.app}
agent="$app/Contents/Library/LoginItems/OpenLogiAgent.app"

say() { printf '\n== %s\n' "$1"; }

say "1. Is the agent running, and which binary is it?"
running=$(pgrep -fl openlogi-agent | head -1 | cut -d' ' -f2-)
if [ -z "$running" ]; then
  echo "   no openlogi-agent process — the GUI cannot show devices without it"
else
  echo "   $running"
  # The identity that matters is the one actually running. A dev bundle, a
  # second copy, or a build in ~/Downloads is a different identity from the
  # installed app, and a grant to one says nothing about the other.
  case "$running" in
    "$app"/*) ;;
    *)
      # `%%` strips at the first ".app/Contents/" -> the outer app bundle;
      # `%` strips at the last one -> the helper bundle it is nested in.
      app=${running%%.app/Contents/*}.app
      agent=${running%.app/Contents/*}.app
      echo "   WARNING: this is not the installed app"
      echo "            grants given to one copy do not apply to another"
      echo "   inspecting the running copy instead: $app"
      ;;
  esac
fi

say "2. Responsible process (must be the agent itself, not the GUI or a terminal)"
pid=$(pgrep -x openlogi-agent | head -1)
if [ -z "${pid:-}" ]; then
  echo "   skipped: agent not running"
elif [ "$(id -u)" -eq 0 ]; then
  launchctl procinfo "$pid" | grep -i responsible || echo "   (no responsible line)"
else
  echo "   needs root; run: sudo launchctl procinfo $pid | grep -i responsible"
fi

say "3. Identities — TCC keys on these, not on the app you see in Finder"
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

say "6. Next step: the agent's own log"
cat <<TXT
   launchd discards the agent's output, so run it in the foreground:

     OPENLOGI_LOG=debug "$agent/Contents/MacOS/openlogi-agent"

   Then classify the first failure you see:
     "HID++ candidate interfaces count=0"      -> device not matched, not a permission problem
     "failed to open HID++ channel ... Failed to open device"
                                               -> Input Monitoring for OpenLogiAgent (or exclusive access)
     "opened HID++ channel" then a probe error -> permissions are fine; async-hid write bug
TXT
