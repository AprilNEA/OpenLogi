# Configuration

How OpenLogi stores its settings. For install and usage, see the
[README](../README.md).

Config is a TOML file, read on startup and written atomically on change:

- macOS & Linux: `$XDG_CONFIG_HOME/openlogi/config.toml` (default `~/.config/openlogi/config.toml`)
- Windows: `%USERPROFILE%\.config\openlogi\config.toml`

Most settings below are managed by the GUI (Settings window, action picker,
DPI / SmartShift / lighting panels), but the file stays hand-editable;
per-application overlays and custom shortcuts are currently authored there.
OpenLogi reloads it on startup. Older files migrate on first load:
`schema_version = 1` (separate `button_bindings` / `gesture_bindings` tables)
into the unified `bindings` map, and pre-v3 model-keyed device tables into
route-keyed ones.

The current schema is `schema_version = 3`. Per-device settings are keyed by
the device's **route identity** — `receiver:<serial>:slot:<n>` for a device
behind a Bolt/Unifying receiver, `direct:<vid>:<pid>:unit:<serial>` for USB /
Bluetooth-direct — so two identical models never share settings. Each device
table also carries an `identity` block (model info and capabilities) that
OpenLogi maintains automatically; leave it alone when hand-editing.

- `bindings` — one entry per rebindable button: a single action, a
  per-direction table for the gesture button, or a per-sector table for the
  MX Master 4's Action Ring pad.
- `per_app_bindings` — overlays keyed by application id (bundle id such as
  `com.microsoft.VSCode` on macOS, `WM_CLASS` on Linux/X11, or a lower-cased
  executable path on Windows) that take precedence while that app is
  frontmost.
- `dpi_presets` — the ordered list cycled by the `CycleDpiPresets` action.
- `smartshift` — wheel mode, sensitivity, and permanent-ratchet state.
- `invert_scroll` — reverse this device's native vertical wheel direction
  without changing the system trackpad direction.
- `lighting` — static RGB colour, brightness (0–100), and on/off for wired
  RGB keyboards.
- `gesture_owner` — which button owns the gesture role, when chosen
  explicitly (otherwise inferred).

The app-wide `[app_settings]` block holds `launch_at_login`,
`check_for_updates`, and `auto_install_updates` (all off by default);
`show_in_menu_bar` (macOS menu bar / Windows tray, ignored on Linux; on by
default); `auto_download_assets` (on by default); `language` (absent = follow
the system locale); `thumbwheel_sensitivity` (default `14`); and the
`appearance` (default `"system"`), `theme_light`, `theme_dark`, and `ui_radius`
presentation settings. The theme and radius overrides are absent by default.

```toml
schema_version = 3
selected_device = "receiver:97b76948a846c55a:slot:2"

[app_settings]
launch_at_login = true
check_for_updates = false
auto_install_updates = false
show_in_menu_bar = true
auto_download_assets = true
language = "en"
thumbwheel_sensitivity = 14
appearance = "system"
# Optional presentation overrides (omit to use the theme defaults):
# theme_light = "OpenLogi Light"
# theme_dark = "OpenLogi Dark"
# ui_radius = 6

[devices."receiver:97b76948a846c55a:slot:2"]
dpi_presets = [800, 1600, 3200]

[devices."receiver:97b76948a846c55a:slot:2".bindings]
Back = "BrowserBack"
Forward = "BrowserForward"

# Gesture button: one action per swipe direction; Click = plain press.
[devices."receiver:97b76948a846c55a:slot:2".bindings.GestureButton]
Click = "MissionControl"
Up = "MissionControl"
Down = "AppExpose"
Left = "PreviousDesktop"
Right = "NextDesktop"

# Action Ring pad (MX Master 4): one action per ring sector, compass-named
# clockwise from the top. With this table a tap opens the on-screen ring;
# replace the whole table with a single action (ActionRing = "Copy") to make
# a tap fire that action directly instead.
[devices."receiver:97b76948a846c55a:slot:2".bindings.ActionRing]
North = "Copy"
NorthEast = "CaptureRegion"
East = "Redo"
SouthEast = "PlayPause"
South = "Paste"
SouthWest = "ShowDesktop"
West = "Undo"
NorthWest = "MissionControl"

# Per-app overlay: Back becomes Undo only while VS Code is frontmost.
[devices."receiver:97b76948a846c55a:slot:2".per_app_bindings."com.microsoft.VSCode"]
Back = "Undo"

[devices."receiver:97b76948a846c55a:slot:2".lighting]
enabled = true
color = "ff0000"
brightness = 80
```

Action names are the catalog's variant names (`LeftClick`, `MouseBack`,
`Copy`, `PlayPause`, `CycleDpiPresets`, …). Custom keyboard shortcuts are
currently hand-authored as a `CustomShortcut` table in the TOML file.
