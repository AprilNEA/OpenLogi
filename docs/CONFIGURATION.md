# Configuration

How OpenLogi stores its settings. For install and usage, see the
[README](../README.md).

Config is a TOML file, read on startup and written atomically on change. Before
the first save in each app process, OpenLogi preserves the previous file as
`config.toml.backup.1` and rotates up to `config.toml.backup.5`.

- macOS & Linux: `$XDG_CONFIG_HOME/openlogi/config.toml` (default `~/.config/openlogi/config.toml`)
- Windows: `%USERPROFILE%\.config\openlogi\config.toml`

Most settings below are managed by the GUI (Settings window, action picker,
DPI / SmartShift / lighting panels), but the file stays hand-editable;
per-application overlays and custom shortcuts are currently authored there.
OpenLogi reloads it on startup. Older schemas are migrated on load, including
`schema_version = 1` files that split button and gesture bindings.

Per-device settings are keyed by physical identity, such as
`receiver:aabbccdd:slot:1` for a receiver-connected device. This keeps two
mice of the same model independent:

- `bindings` — one entry per rebindable button: either a single action, or a
  per-direction table for the gesture button.
- `per_app_bindings` — overlays keyed by application id (bundle id such as
  `com.microsoft.VSCode` on macOS, `WM_CLASS` on Linux/X11, or a lower-cased
  executable path on Windows) that take precedence while that app is
  frontmost. Windows also accepts `exe:<filename>.exe`, for example
  `exe:sharex.exe`, as a stable fallback for Store and self-updating apps. An
  exact path entry wins when both forms exist.
- `action_ring` — the enabled state, haptic-feedback preference, default
  eight-slot layout, and complete per-application layouts.
- `dpi_presets` — the ordered list cycled by the `CycleDpiPresets` action.
- `smartshift` — wheel mode, sensitivity, and permanent-ratchet state.
- `invert_scroll` — reverse this device's native vertical wheel direction
  without changing the system trackpad direction.
- `lighting` — static RGB colour, brightness (0–100), and on/off for wired
  RGB keyboards.
- `light` — standalone-light power, normalized brightness, and temperature.
  Set `auto_camera = true` on macOS to turn the light on while any camera is in
  use and off when camera use stops; the manual power preference and the other
  light settings remain independent.
- `gesture_owner` — which button owns the gesture role, when chosen
  explicitly (otherwise inferred).
- `host_switch_targets` — on a compatible keyboard, physical config keys of
  mice that should follow its Easy-Switch channel. Both devices must already
  be paired on corresponding channels. The keyboard's host controls and every
  target must expose the HID++ features needed for host switching. Configure
  the link on every computer from which the keyboard may initiate a switch.
- `fn_lock` — keyboards only: `true` makes the F-row send F1–F12 without
  holding Fn, `false` keeps the printed media/shortcut functions. Absent
  means the keyboard's own state is left alone. Re-applied on reconnect.

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

[devices.2b042]
dpi_presets = [800, 1600, 3200]

# Put this on the keyboard's physical device entry. Values are the physical
# keys of the mice that should follow it; use the exact keys already present
# under [devices] in your generated config.
[devices."receiver:aabbccdd:slot:1"]
host_switch_targets = ["receiver:aabbccdd:slot:2"]

[devices."receiver:aabbccdd:slot:1".action_ring]
enabled = true
haptics = true

# Each populated slot owns its action, an optional presentation icon, and an
# optional hover label. Omit `icon` to use the action's normal icon; omit
# `label` to use the action's generic name (useful for `RunShellCommand`
# slots, which otherwise all read "Run Command"); omit the slot to leave it
# empty. Custom labels always render exactly as written — they are never
# passed through localization, even when they match a built-in action name.
[devices."receiver:aabbccdd:slot:1".action_ring.default.slots]
Top = { action = "Copy", icon = "Keyboard" }
TopRight = { action = "Paste", label = "Paste It" }
Right = { action = "BrowserForward" }
BottomRight = { action = "NextTab" }
Bottom = { action = "ShowDesktop", icon = "Applications" }
BottomLeft = { action = "PrevTab" }
Left = { action = "BrowserBack" }
TopLeft = { action = "Cut" }

# A per-app ring is a complete layout, not a sparse overlay.
[devices."receiver:aabbccdd:slot:1".action_ring.per_app."com.microsoft.VSCode".slots]
Top = { action = "Copy" }
TopRight = { action = "Paste" }
Right = { action = "Redo" }
BottomRight = { action = "NextTab" }
Bottom = { action = "ShowDesktop" }
BottomLeft = { action = "PrevTab" }
Left = { action = "Undo" }
TopLeft = { action = "Cut" }

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

# Stable Windows executable-name selector (exact paths still take precedence).
[devices.2b042.per_app_bindings."exe:sharex.exe"]
MiddleClick = { CustomShortcut = { modifiers = 0, key_code = 122, display = "F1" } }

# Actions Ring slots couple an executable action with an optional custom icon.
[devices.2b042.action_ring]
enabled = true
haptics = true

[devices.2b042.action_ring.default.slots]
Top = { action = "Copy", icon = "Keyboard" }
Right = { action = { OpenApplication = { path = "/Applications/Safari.app", display_name = "Safari" } }, icon = "Applications" }
Bottom = { action = "ShowDesktop" }

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

# Standalone light (for example, a Litra Glow). The GUI writes this block under
# the serial-backed physical key; `openlogi light list` shows its HID tuple and
# identity when diagnosing discovery.
# A serial-bearing Litra key looks like:
# [devices."raw:046d:c900:ff43:0202:serial:YOUR-SERIAL".light]
# If the HID backend exposes only a transient OS-node identity, OpenLogi does
# not persist that key; reconnect persistence then requires a device serial.
[devices."<raw-device-key>".light]
enabled = true
auto_camera = true
brightness_percent = 65
temperature_kelvin = 4600
```

Action names are the catalog's variant names (`LeftClick`, `MouseBack`,
`Copy`, `PlayPause`, `CycleDpiPresets`, …). `ShowActionsRing` opens the ring;
a detected Haptic Sense Panel uses it by default. Ring slots reject
`ShowActionsRing` itself to prevent recursive sessions. `OpenApplication`
accepts an application, folder, filesystem path, or URL. A leading `~` is
expanded when the action runs; for example:

```toml
Top = { action = { OpenApplication = { path = "/Applications/Safari.app", display_name = "Safari" } } }
Bottom = { action = { OpenApplication = { path = "~/Downloads", display_name = "Downloads" } } }
```

`CustomShortcut` stores a platform-neutral textual chord, for example:

```toml
Top = { action = { CustomShortcut = "Cmd+Shift+P" } }
```

The GUI accepts chords such as `Cmd+Shift+P`, `Ctrl+Alt+Left`, or `F5`. It also
lets each ring slot keep its action-derived icon or choose a custom icon from
the built-in gallery.
