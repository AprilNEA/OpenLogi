# OpenLogi Frontmost Window — GNOME Shell extension

GNOME (Mutter) does not let ordinary clients see which window is focused on
Wayland, and it implements neither `wlr-foreign-toplevel` nor a focused-window
portal. This minimal extension bridges that gap: it exports the WM_CLASS of the
focused window over D-Bus and emits every native focus change so OpenLogi's
`gnome-shell` backend can drive per-app mouse-profile switching without
polling.

The extension also excludes `openlogi-action-ring` windows from GNOME's window
animations. GNOME's opening animation scales the surface, changing its local
pointer coordinates while it appears. The ring anchors to the first pointer
report, so without this exception it can stay far from the cursor after the
animation ends, especially near screen edges ([#1210](https://github.com/AprilNEA/OpenLogi/issues/1210)).
Other windows retain their normal animation policy; disabling the extension
restores GNOME's original policy. The global animation setting is never changed.

No titles, window contents, or input are read, and the extension adds no UI.

## D-Bus surface

- name: `org.openlogi.Frontmost`
- path: `/org/openlogi/Frontmost`
- method: `GetFocusedWmClass() -> s` (empty string when nothing is focused)
- signal: `FocusedWmClassChanged(s)` (emitted on each GNOME focus change)

## Install

```sh
UUID=openlogi-frontmost@openlogi.dev
DEST="$HOME/.local/share/gnome-shell/extensions/$UUID"
mkdir -p "$DEST"
cp metadata.json extension.js ringAnimation.js "$DEST"/
```

On Wayland the shell cannot be reloaded in place, so **log out and back in** to
let GNOME pick up the newly added extension, then enable it:

```sh
gnome-extensions enable "$UUID"
gnome-extensions info "$UUID"   # State should be ACTIVE
```

## Verify

```sh
# Introspect the service:
busctl --user introspect org.openlogi.Frontmost /org/openlogi/Frontmost

# Focus a window, then query it:
gdbus call --session \
  -d org.openlogi.Frontmost \
  -o /org/openlogi/Frontmost \
  -m org.openlogi.Frontmost.GetFocusedWmClass

# Watch focus changes:
gdbus monitor --session \
  -d org.openlogi.Frontmost \
  -o /org/openlogi/Frontmost
```

If the call prints the focused window's WM_CLASS and the monitor reports
`FocusedWmClassChanged` while switching windows, OpenLogi's GNOME backend will
pick it up automatically the next time the hook starts.

With GNOME animations enabled, open the Actions Ring repeatedly over a native
Wayland application, first near the center and then near each edge. Its center
should match the pointer away from edges; near an edge it should move only far
enough inward to keep the ring visible. Hover and click a slot to verify its
hit target matches the drawing. Ordinary application windows should still
animate. Disable the extension to verify that its animation override is removed.

The animation-policy regression tests need Node.js 22 or newer and no running shell:

```sh
node --test crates/openlogi-hook/gnome-shell-extension/tests/ringAnimation.test.mjs
```

Run that command from the repository root. It does not replace the live GNOME
placement check above.

## Notes

- The `shell-version` list in `metadata.json` covers GNOME 45–50. Newer GNOME
  releases may need an added entry; the API used here (`Gio.DBusExportedObject`,
  `global.display.focus_window`, `Meta.Window.get_wm_class`) has been stable
  across these versions.
- The extension name/UUID and the D-Bus name (`org.openlogi.*`) are placeholders
  that should track the project's namespace; if they change, update the matching
  constants in `crates/openlogi-hook/src/linux/gnome_shell.rs`.
- The scoped animation exception uses GNOME Shell's `_shouldAnimateActor` hook
  through its `InjectionManager`. When adding a new shell version, check that
  hook and run the live placement check as well as the policy tests.
