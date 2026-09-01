# Configuration

OpenLogi stores settings as plain TOML. The GUI and agent read the same file:

- macOS and Linux: `$XDG_CONFIG_HOME/openlogi/config.toml` (normally
  `~/.config/openlogi/config.toml`)
- Windows: `%USERPROFILE%\.config\openlogi\config.toml`

The complete, tested example is [config.example.toml](config.example.toml).
Copy only the sections you need and replace its example physical device keys
with keys already written by OpenLogi for your devices.

## Editing and recovery

The GUI writes atomically and keeps `config.toml.backup.1` through
`config.toml.backup.5`. Existing comments and formatting are retained when the
GUI updates known fields.

The schema is strict: misspelled, obsolete, and out-of-range fields stop the
config from loading instead of silently selecting a default or disappearing on
the next save. The GUI then opens in read-only mode and shows the exact TOML
error. Fix the file and relaunch OpenLogi.

If the file changes in an editor while the GUI is open, the next GUI save is
refused rather than overwriting the external edit. Relaunch to load that
revision. Opening the GUI also tells the resident agent to reload the current
file, so hand edits and runtime behavior converge immediately.

Schema versions newer than the running build are rejected before their fields
are parsed. v1 binding maps and the v2–v3 gesture-owner layout migrate on load.
The v3 physical-device-key transition cannot safely assign older model-scoped
device settings when two identical devices exist, so v2 model-key entries must
be copied manually to the generated physical keys. Pre-v7 thumb-wheel scroll
pairs on the old defaults are migrated in both device and per-application
profiles so they keep their native direction.

## Shape

`schema_version` is required and currently `7`. `selected_device` is an
optional physical device key.

`[app_settings]` contains application-wide preferences:

- startup, update, menu-bar / tray, input-capture, and asset-download toggles
- `asset_source`: `automatic`, `openlogi`, `cloudflare`, or `fastly`
- `language`, `appearance`, `device_view_mode` (`grid`, `list`, or `carousel`),
  optional theme names, and optional UI radius
- `smooth_scroll` toggles finite animation for traditional mouse-wheel input
- `vertical_scroll_sensitivity`, from `1` through `100` (`14` is 1×);
  continuous trackpad input remains native
- `thumbwheel_sensitivity`, from `1` through `100` (`14` is 1×)

`[devices."<physical-key>"]` contains per-device state. Receiver keys look like
`receiver:<receiver-id>:slot:<number>`; direct, raw-HID, and camera devices use
other generated keys. Do not substitute a model id such as `2b042`.

A camera without a USB serial has no unique port-stable identity. Its
`custom_name` key therefore follows the OS capture id so two same-model cameras
remain distinguishable; moving it to another USB port may require naming it
again.

Common device fields are:

- `custom_name`, `enabled`, `dpi`, `dpi_presets`, thumb-wheel sensitivity,
  scroll inversion, and scroll resolution
- `bindings`: a button maps to one action, an independent short/long action
  pair, or a gesture-direction map.
  `Thumbwheel` is the thumb wheel's capacitive tap — it has no GUI control and
  stays inert unless bound here, because the wheel reports taps from incidental
  thumb contact as well as from deliberate ones
- `per_app_bindings`: sparse action overlays keyed by macOS bundle id, Linux
  application id, exact lower-cased Windows executable path, or
  `exe:<filename>.exe`. The Buttons panel edits these under its Profile
  selector, which offers applications the agent has seen in front — the only
  identifiers guaranteed to match, since the four platforms name applications
  differently and a profile authored under one namespace will not match under
  another. An overlay holds one action per button; gesture-direction maps live
  in `bindings`
- `action_ring`: default and complete per-application eight-slot layouts
- `lighting`, `smartshift`, standalone `light`, and camera controls / profiles
- `host_switch_targets` and `fn_lock` for compatible keyboards
- `identity` and `disabled_gestures`, which are application-managed metadata

`[keyboard.bindings]` contains global key triggers such as `f1` or
`shift+command+f5`. Supported trigger modifiers are `shift`, `control`,
`option`, and `command`; aliases such as `ctrl`, `alt`, and `cmd` are accepted.

## Actions

Action names are the serialized Rust variant names, including `Copy`,
`BrowserBack`, `PlayPause`, `CycleDpiPresets`, `SmartZoom`, and
`ShowActionsRing`.
Payload actions use a one-key inline table:

```toml
Back = { CustomShortcut = "Cmd+Shift+P" }
Forward = { HoldShortcut = "Ctrl+Space" }
MiddleClick = { OpenApplication = { path = "~/Downloads", display_name = "Downloads" } }
DpiToggle = { short = "ShowDesktop", long = "MissionControl" }
```

`SmartZoom` is offered by the action picker only on macOS. It emits one native
Smart Zoom gesture at the pointer, leaving Safari, Chrome, Preview, or another
receiving application to decide what zoom behavior follows. The action remains
valid in transferred configs on Linux and Windows, where it safely does nothing.

`CustomShortcut` emits an immediate key-down/key-up pair. `HoldShortcut` keeps
the chord down until the originating physical button is released, and also
releases it if capture is interrupted, the binding becomes invalid, or the
agent shuts down. Use it for push-to-talk and other hold-to-activate controls.

A `{ short = ..., long = ... }` binding waits for the button's outcome instead
of firing on press. Releasing before 500 ms fires `short`; keeping the button
down for 500 ms fires `long` exactly once, and the later release does not also
fire `short`. If capture is interrupted, the binding changes, or the agent
shuts down before either outcome, neither action fires. A source that can only
report an instantaneous button pulse falls back to `short`. `long` may itself
be a `HoldShortcut`, in which case its chord stays down from the 500 ms
threshold until the physical release.

Long-press pairs currently apply to global device `bindings` and are authored
in TOML. The GUI presents their `short` action; changing that button in the GUI
replaces the whole pair with the selected single action. `per_app_bindings` and
`keyboard.bindings` remain single-action maps.

An Actions Ring entry wraps the action and may add an icon or literal label:

```toml
Top = { action = { CustomShortcut = "Cmd+Shift+P" }, icon = "Keyboard", label = "Command Palette" }
```

`ShowActionsRing` is rejected inside a ring slot to prevent recursive rings.
