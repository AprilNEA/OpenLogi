# Configuration

How OpenLogi stores its settings. For install and usage, see the
[README](../README.md).

Config is a TOML file, read on startup and written atomically on change:

- macOS & Linux: `$XDG_CONFIG_HOME/openlogi/config.toml` (default `~/.config/openlogi/config.toml`)
- Windows: `%USERPROFILE%\.config\openlogi\config.toml`

Everything below is managed by the GUI (Settings window, action picker, DPI /
SmartShift / lighting panels), but the file stays hand-editable; OpenLogi
reloads it on startup. Older `schema_version = 1` files (separate
`button_bindings` / `gesture_bindings` tables) are migrated to the unified
`bindings` map on first load.

Per-device settings are keyed by the HID++ identifier (e.g. `2b042` for an
MX Master 4):

- `bindings` — one entry per rebindable button: either a single action, or a
  per-direction table for the gesture button.
- `per_app_bindings` — overlays keyed by application id (bundle id such as
  `com.microsoft.VSCode` on macOS, `WM_CLASS` on Linux/X11) that take
  precedence while that app is frontmost.
- `dpi_presets` — the ordered list cycled by the `CycleDpiPresets` action.
- `lighting` — static RGB colour, brightness (0–100), and on/off for wired
  RGB keyboards.
- `gesture_owner` — which button owns the gesture role, when chosen
  explicitly (otherwise inferred).
- `fn_lock` — keyboards only: `true` makes the F-row send F1–F12 without
  holding Fn, `false` keeps the printed media/shortcut functions. Absent
  means the keyboard's own state is left alone. Re-applied on reconnect.

The app-wide `[app_settings]` block holds `launch_at_login`,
`check_for_updates` (both off by default), `show_in_menu_bar` (macOS-only)
and `language` (absent = follow the system locale).

```toml
schema_version = 2
selected_device = "2b042"

[app_settings]
launch_at_login = true
language = "en"

[devices.2b042]
dpi_presets = [800, 1600, 3200]

[devices.2b042.bindings]
Back = "BrowserBack"
Forward = "BrowserForward"

# Gesture button: one action per swipe direction; Click = plain press.
[devices.2b042.bindings.GestureButton]
Click = "MissionControl"
Up = "MissionControl"
Down = "AppExpose"
Left = "PreviousDesktop"
Right = "NextDesktop"

# Per-app overlay: Back becomes Undo only while VS Code is frontmost.
[devices.2b042.per_app_bindings."com.microsoft.VSCode"]
Back = "Undo"

[devices.2b042.lighting]
enabled = true
color = "ff0000"
brightness = 80

# Keyboard F-row keys (Signature-series layout): a bound key is diverted
# over HID++ and dispatches its action; an unbound key keeps its native
# firmware function. Key names: KeySearch, KeyDictation, KeyEmoji,
# KeyScreenCapture, KeyMicMute, KeyPlayPause, KeyMute, KeyVolumeDown,
# KeyVolumeUp.
[devices.2b372]
fn_lock = false

[devices.2b372.bindings]
KeySearch = "MissionControl"
KeyScreenCapture = "Sleep"
```

Action names are the catalog's variant names (`LeftClick`, `MouseBack`,
`Copy`, `PlayPause`, `CycleDpiPresets`, …); recorded keyboard shortcuts
serialize as a `CustomShortcut` table written by the GUI's recorder.
