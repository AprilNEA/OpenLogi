# OpenLogi Flow Edges — KWin script

On a KDE Wayland session an ordinary client cannot read the cursor position,
so OpenLogi's Flow cannot see the cursor reach a screen edge. The compositor
can: KWin calls back when the cursor pushes into an edge. This script forwards
that call to the OpenLogi agent, which then decides whether a Flow peer sits
beyond the edge.

The script reads the cursor position and the screen layout and nothing else.
It never captures, moves or blocks input, so a fault in it cannot take the
keyboard or mouse away. When the agent is not running, its D-Bus call goes
nowhere and nothing happens.

KWin limits how often an edge fires (about once a second), and the script adds
no timers of its own.

## D-Bus surface (served by the agent)

- name: `org.openlogi.Edge`
- path: `/org/openlogi/Edge`
- method: `Crossed(s side, s position, s displays)`
  - `side`: `left`, `right`, `top` or `bottom`
  - `position`: `"x,y"`, the cursor in KWin's logical coordinates
  - `displays`: `"x,y,w,h;x,y,w,h;…"`, every screen in the same space

## Install

```sh
kpackagetool6 --type KWin/Script --install openlogi-edge
kwriteconfig6 --file kwinrc --group Plugins --key openlogi-edgeEnabled true
qdbus6 org.kde.KWin /KWin reconfigure
```

To update after changing the script, use `--upgrade` instead of `--install`.

## Remove

```sh
kwriteconfig6 --file kwinrc --group Plugins --key openlogi-edgeEnabled false
qdbus6 org.kde.KWin /KWin reconfigure
kpackagetool6 --type KWin/Script --remove openlogi-edge
```

## Verify

```sh
# The agent's service exists while the agent runs on a KDE Wayland session:
busctl --user introspect org.openlogi.Edge /org/openlogi/Edge

# Push the cursor into an edge and watch the calls arrive:
busctl --user monitor org.openlogi.Edge
```

The edges the script registers are KWin's own screen edges. If you also use one
of them for a Screen Edges action, check that the two do not get in each
other's way, and disable that action if they do.
