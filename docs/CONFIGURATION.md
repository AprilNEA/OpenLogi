# Configuration

How OpenLogi stores its settings. For install and usage, see the
[README](../README.md).

Config is a TOML file, read on startup and written atomically on change:

- macOS & Linux: `$XDG_CONFIG_HOME/openlogi/config.toml` (default `~/.config/openlogi/config.toml`)
- Windows: `%USERPROFILE%\.config\openlogi\config.toml`

Most settings below are managed by the GUI (Settings window, action picker,
DPI / SmartShift / lighting panels), but the file stays hand-editable;
per-application overlays and custom shortcuts are currently authored there.
OpenLogi reloads it on startup. Older schemas are migrated on load, including
`schema_version = 1` files that split button and gesture bindings.

Per-device settings are keyed by physical identity, such as
`receiver:aabbccdd:slot:1` for a receiver-connected device. This keeps two mice
of the same model independent:

- `bindings` — one entry per rebindable button: either a single action, or a
  per-direction table for the gesture button.
- `per_app_bindings` — overlays keyed by application id (bundle id such as
  `com.microsoft.VSCode` on macOS, `WM_CLASS` on Linux/X11, or a lower-cased
  executable path on Windows) that take precedence while that app is
  frontmost.
- `action_ring` — the enabled state, haptic-feedback preference, default
  eight-slot layout, and complete per-application layouts.
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
schema_version = 3
selected_device = "receiver:aabbccdd:slot:1"

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

[devices."receiver:aabbccdd:slot:1"]
dpi_presets = [800, 1600, 3200]

[devices."receiver:aabbccdd:slot:1".bindings]
Back = "BrowserBack"
Forward = "BrowserForward"

# Gesture button: one action per swipe direction; Click = plain press.
[devices."receiver:aabbccdd:slot:1".bindings.GestureButton]
Click = "MissionControl"
Up = "MissionControl"
Down = "AppExpose"
Left = "PreviousDesktop"
Right = "NextDesktop"

# Per-app overlay: Back becomes Undo only while VS Code is frontmost.
[devices."receiver:aabbccdd:slot:1".per_app_bindings."com.microsoft.VSCode"]
Back = "Undo"

[devices."receiver:aabbccdd:slot:1".action_ring]
enabled = true
haptics = true

[devices."receiver:aabbccdd:slot:1".action_ring.default.slots]
Top = "Copy"
TopRight = "Paste"
Right = "BrowserForward"
BottomRight = "NextTab"
Bottom = "ShowDesktop"
BottomLeft = "PrevTab"
Left = "BrowserBack"
TopLeft = "Cut"

# Optional presentation icons; omitted slots use their action's normal icon.
[devices."receiver:aabbccdd:slot:1".action_ring.default.icons]
Top = "Keyboard"
Bottom = "Applications"

# A per-app ring is a complete layout, not a sparse overlay.
[devices."receiver:aabbccdd:slot:1".action_ring.per_app."com.microsoft.VSCode".slots]
Top = "Copy"
TopRight = "Paste"
Right = "Redo"
BottomRight = "NextTab"
Bottom = "ShowDesktop"
BottomLeft = "PrevTab"
Left = "Undo"
TopLeft = "Cut"

[devices."receiver:aabbccdd:slot:1".lighting]
enabled = true
color = "ff0000"
brightness = 80
```

Action names are the catalog's variant names (`LeftClick`, `MouseBack`,
`Copy`, `PlayPause`, `CycleDpiPresets`, …). `ShowActionsRing` opens the ring;
a detected Haptic Sense Panel uses it by default. Ring slots reject
`ShowActionsRing` itself to prevent recursive sessions. `OpenApplication`
accepts an application, folder, filesystem path, or URL. A leading `~` is
expanded when the action runs; for example:

```toml
Top = { OpenApplication = { path = "/Applications/Safari.app", display_name = "Safari" } }
Bottom = { OpenApplication = { path = "~/Downloads", display_name = "Downloads" } }
```

The GUI can create `CustomShortcut` actions from entries such as
`Cmd+Shift+P`, `Ctrl+Alt+Left`, or `F5`. It also lets each ring slot keep its
action-derived icon or choose a custom icon from the built-in gallery.
