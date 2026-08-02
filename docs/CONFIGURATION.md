# Configuration

How OpenLogi stores its settings. For install and usage, see the
[README](../README.md).

Config is a TOML file, read on startup and written atomically on change:

- macOS & Linux: `$XDG_CONFIG_HOME/openlogi/config.toml` (default `~/.config/openlogi/config.toml`)
- Windows: `%USERPROFILE%\.config\openlogi\config.toml`

Most settings below are managed by the GUI (Settings window, action picker,
DPI / SmartShift / lighting panels), but the file stays hand-editable;
per-application overlays and custom shortcuts are currently authored there.
OpenLogi reloads it on startup. Older `schema_version = 1` files (separate
`button_bindings` / `gesture_bindings` tables) are migrated to the unified
`bindings` map on first load.

Per-device settings are keyed by the HID++ identifier (e.g. `2b042` for an
MX Master 4):

- `bindings` — one entry per rebindable button: either a single action, or a
  per-direction table for the gesture button.
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
default); `capture_mouse_events` (on by default; set to `false` to keep the
agent from installing the OS-level mouse hook at all — button remapping stops
working, but no input device is grabbed or intercepted; DPI, SmartShift, and
the other HID++-side features keep working; takes effect on agent restart);
`auto_download_assets` (on by default); `language` (absent = follow the system
locale); `thumbwheel_sensitivity` (default `14`); and the `appearance` (default
`"system"`), `theme_light`, `theme_dark`, and `ui_radius` presentation
settings. The theme and radius overrides are absent by default.

```toml
schema_version = 2
selected_device = "2b042"

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
```

Action names are the catalog's variant names (`LeftClick`, `MouseBack`,
`Copy`, `PlayPause`, `CycleDpiPresets`, …). Custom keyboard shortcuts are
currently hand-authored as a `CustomShortcut` table in the TOML file.
