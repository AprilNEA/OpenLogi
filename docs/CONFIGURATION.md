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

When `config.toml` is a symbolic link, saves update its target and preserve
the link. A missing target and its parent directories are created. Backups stay
beside the configured `config.toml`, while a persistent `.lock` file stays beside
the target so different links to the same file share the writer lock.

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
profiles so they keep their native direction. v9 adds the `[apps]` alias table;
v8 files load unchanged.
v10 adds the per-device `disabled_keys` policy. Older readers reject a v10
header before parsing its fields. A v9 file without the policy still loads.

## Shape

`schema_version` is required and currently `10`. `selected_device` is an
optional physical device key.

`[app_settings]` contains application-wide preferences:

- startup, update, menu-bar / tray, input-capture, and asset-download toggles
- `asset_source`: `automatic`, `openlogi`, `cloudflare`, or `fastly`
- `language`, `appearance`, `device_view_mode` (`grid`, `list`, or `carousel`),
  optional theme names, and optional UI radius
- `smooth_scroll` toggles finite animation for traditional mouse-wheel input. With it
  off, a diverted horizontal thumb wheel still scrolls as a phased gesture (ended after
  120 ms idle) so apps that reveal content on horizontal swipes, like Messages, react;
  vertical wheel output stays a plain wheel. A mouse whose `smooth_wheel` is on
  eases its main wheel either way
- `vertical_scroll_sensitivity`, from `1` through `100` (`14` is 1×);
  continuous trackpad input remains native
- `thumbwheel_sensitivity`, from `1` through `100` (`14` is 1×)
- `mouse_profile_target`: `pointer` (default, including existing configs that
  omit this preference) or `focused`. Mouse button profiles follow the window
  under the pointer; the desktop uses the global bindings. Keyboard profiles
  continue to follow the focused application. Pointer targeting is supported
  on macOS, Windows, and X11; unsupported sessions such as Wayland use the
  focused application, and so does a pointer over a surface OpenLogi cannot
  identify, such as a system overlay, or a lookup that fails. An unidentified
  surface uses the focused application's profile rather than the desktop's;
  with no focused application there is no such profile and the global bindings
  apply, as in focused mode. OpenLogi never activates a background window to
  send a shortcut: over an identified window or the desktop, mouse
  bindings that produce keystrokes or run workflows are skipped unless the
  hovered window is focused. Global actions such as desktop switching can run
  without changing application focus.
- `low_battery_notifications` (default `true`): the agent shows a notice at the
  bottom of the screen when a connected device's battery runs low. Low means
  20% or less while the device is not charging, or a low or critical level from
  a device that reports no percentage. Each discharge is announced once. The
  device is announced again only after it charges, reads 30% or more, or
  reports a good or full level. A notice closes after 8 seconds or when it is
  clicked. With the setting off the agent posts no notice, but it still notes
  when a device charges, so the next discharge is announced once the setting
  is back on. A notice already on screen still closes on its own.
- `lock_key_notifications` (default `true`): when a keyboard with the HID++
  `LockKeyState` feature (`0x4220`) reports Caps Lock turning on or off, the
  agent shows an indicator in the centre of the screen for 1.5 seconds. On
  Windows and Linux it does the same for Num Lock and Scroll Lock. macOS gives
  those two keys no meaning, so they show nothing there. A change replaces the
  one on screen. The state a keyboard reports when it connects is not
  announced. With the setting off the agent shows nothing, and an indicator
  already on screen still closes on its own. Fn Lock is not shown.
- `animate_desktop_switch` (default `true`, macOS only): Previous Desktop and
  Next Desktop slide to the neighbouring Space. The agent types the "Move left
  a space" or "Move right a space" shortcut from System Settings → Keyboard →
  Keyboard Shortcuts → Mission Control, so the Dock plays the slide it plays
  for the keyboard. When that shortcut is off, the agent posts the slide the
  way a three-finger swipe does: 20 frames, one every 16.6 ms, and each moving
  frame covers 15% of the distance left; the Dock finishes the slide. That
  action returns when the last frame is posted, about 315 ms later. With the
  setting off, the agent posts two frames and the Dock changes Space at once.
  A change applies from the next switch. Windows and Linux ignore the setting.

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
- `smooth_wheel` for mice with a HID++ `0x2121` wheel: `true` smooths that
  mouse's main wheel in software, as Logi Options+ "Smooth scrolling" does,
  whatever `smooth_scroll` says. The agent switches the wheel to diverted
  reporting and posts eased scrolling for each report, applying the mouse's
  scroll inversion and `vertical_scroll_sensitivity`. While it is on,
  `scroll_resolution` only sets how finely the wheel reports: each report is
  converted back to notches, so a notch scrolls the same distance at either
  resolution, and `vertical_scroll_sensitivity` is what makes it faster. It
  needs the input access the mouse hook needs (Accessibility on macOS) and
  `capture_mouse_events` on; without them the wheel stays native. The agent
  hands the wheel back to native reporting when the switch goes off, the mouse
  is disabled, or the agent exits. If the agent crashes first, the wheel does
  not scroll until the agent runs again (the launch-at-login service restarts
  it) or the mouse reconnects, which drops the diversion. The desktop's
  Scrolling card offers it as **Smooth wheel** when the current connection has
  `0x2121`; a smooth wheel turned on over another connection can still be turned
  off there. Absent or `false` leaves the wheel native
- `bindings`: a button maps to one action, an independent short/long action
  pair, or a gesture-direction map.
  `Thumbwheel` is the thumb wheel's capacitive tap — it has no GUI control and
  stays inert unless bound here, because the wheel reports taps from incidental
  thumb contact as well as from deliberate ones.
  Keyboard keys are named by their HID++ `0x1b04` control: a catalog name such
  as `KeyScreenCapture`, `KeyCalculator` or `KeyLock`, or `control:0x<cid>` for
  any control `openlogi diag controls` reports as divertable. A bound key is
  diverted to OpenLogi while the agent runs; an unbound key keeps its firmware
  function. An explicit `None` action instead diverts and suppresses the key;
  it is **not** the same as removing the entry. In the Keys panel, **Native
  function** removes the device-default assignment. **Use the default profile**
  in an app scope removes only that app's override and inherits the device
  default, which may itself be assigned. **Do Nothing** keeps an explicit `None`.
  The selected row shows the assignment in the scope being edited, not the
  inherited action. For example, when an app inherits **Copy**, only **Use the
  default profile** is selected. Choosing **Copy** creates an explicit app
  override even though the effective action is unchanged; choosing **Copy**
  again or **Use the default profile** removes that override. The keyboard
  illustration continues to show the effective action, including inheritance.
  To undo an older GUI's Native selection that saved `None`, select Native again;
  existing `None` entries are not migrated because they may be intentional.
  Naming one control under both spellings in the same table is an error, not a merge
- `per_app_bindings`: sparse action overlays keyed by macOS bundle id (or the
  exact executable path for applications without a bundle id), Linux application
  id, exact lower-cased Windows executable path, or `exe:<filename>.exe`. macOS
  paths retain their original spelling and case. The Buttons panel edits these
  under its Profile selector. After bringing an application to the foreground,
  choose it from Recent applications to use the identifier the agent observed.
  Identifiers differ between platforms, so a profile keyed by one of them does
  not match on another. Key the profile `app:<alias>` instead to share it
  across platforms (see [Application aliases](#application-aliases)). An
  overlay holds one action per button; gesture-direction maps live in
  `bindings`
- `action_ring`: default and complete per-application eight-slot layouts;
  `action_ring.per_app` takes the same selectors as `per_app_bindings`,
  including the Windows `exe:<filename>` fallback and `app:<alias>` keys
- `lighting`, `smartshift`, and camera controls / profiles
- standalone `light`: the agent writes it when the light connects, when a save
  changes it, and, with `auto_camera` on, when camera use starts or stops.
  Saving another setting leaves a change made with the light's own buttons in
  place. On a light the agent reaches over HID++ `0x1990`, a change made with
  its buttons or by another program also replaces the saved value while the
  agent runs, so a reconnect writes the new value back, as in Logi Options+.
  The agent publishes the change to clients and keeps it until a save or a
  write from OpenLogi sets that control; a write that fails leaves it, unless
  a reload during the write replaced it. The desktop app saves the change
  as the light's setting. While one of its own
  writes to that light is in flight, it waits for the write to settle and
  saves only what the agent still reports. It writes each control it changes
  before it saves it, so its save of a value the light reported earlier leaves
  a newer change made on the light in place. A control set without a write, as
  on an offline light, replaces the change. A hand edit of this file, or an
  edit with the CLI, is a new choice, even when it matches an earlier change
  on the light. That holds as well for a hand edit made after a save of the
  app and before the agent reloads that save: the agent finds the file
  changed since the save. With `auto_camera` on, a power change on the light
  is a manual choice until the next camera transition, like the power switch
  in the settings app
- `host_switch_targets` for keyboards with Easy-Switch keys (HID++ `0x1814`):
  the config keys of the mice and other devices that move with the keyboard
  when it switches hosts. A linked device moves to its slot whose host name
  (HID++ `0x1815`) matches the keyboard's new slot, and otherwise to the slot
  with the same number. The agent also writes the linked device's `0x1814`
  cookies, so that Logi Options+ on the keyboard's other hosts moves the same
  devices. It does not replace a cookie that names another keyboard. Removing
  a link clears the keyboard's cookie. When the device is offline or the clear
  fails, the agent clears it when the device is next online, unless the agent
  restarts first. A disabled device gets no cookie writes. When it is enabled
  again, the agent writes the cookie changes that it skipped. The agent
  remembers each keyboard's cookie while it runs, so it can also write or
  clear a link when the keyboard is asleep or its receiver is unplugged. After
  a restart, the agent knows the cookie again once the keyboard reports it
- `fn_lock` for compatible keyboards
- `disabled_keys` for keyboards with HID++ `0x4521`: the keys the keyboard
  suppresses in hardware (see [Disabled keys](#disabled-keys)). Absent leaves
  the setting unmanaged; an explicit empty array enables all supported known
  keys
- `keep_os_layout` for keyboards whose key layout software can set (HID++
  `0x4531`): absent or `true` keeps the layout on the host OS, as Logi Options+
  does; `false` leaves the keyboard's layout to its own shortcuts
- `report_rate` for mice with an adjustable report rate (HID++ `0x8060` or
  `0x8061`), in hertz. The agent writes it when the mouse reconnects over a
  connection that offers that rate; absent leaves the mouse's own rate
- `pointer_speed` for mice that scale their own pointer motion instead of
  adjustable DPI (HID++ `0x2205`, for example the MX Ergo and the M590), in
  1/256 steps: `256` is 1×, and the value must be in `32..=768` (0.125× to
  3×), the range Logi Options+ writes. The agent writes it when the mouse
  connects; absent leaves the mouse's own speed. The desktop app sets it with
  the Pointer speed slider, where 50 % is 1×
- `force_sensing` for devices with a force-sensing button whose press force
  software can change (HID++ `0x19c0`, the MX Master 4's Haptic Sense Panel),
  as a raw device value with no unit: lower is a lighter press. The range
  differs from unit to unit (one MX Master 4 reports `4071..=6768` with a
  default of `5089`), so the agent clamps the value into the range the device
  reports, then writes it when the device connects; absent leaves the device's
  own force, and the agent never writes one. The desktop app sets it with the
  Press force slider in the Haptic Sense Panel's inspector on the mouse page,
  which saves the force the device reads back
- `identity` and `disabled_gestures`, which are application-managed metadata

`[keyboard.bindings]` contains global key triggers such as `f1` or
`shift+command+f5`. Supported trigger modifiers are `shift`, `control`,
`option`, and `command`; aliases such as `ctrl`, `alt`, and `cmd` are accepted.

### Disabled keys

The Device tab shows a **Disabled keys** card only when the device advertises
HID++ `0x4521`. It reads the supported mask from the keyboard and offers only
the supported keys: Caps Lock, Num Lock, Scroll Lock, Insert, and Windows /
Command (labelled for the host OS). A switch **on disables that key**. This is
a firmware setting, not key remapping: disabling Caps Lock also suppresses a
host-side action assigned to it, such as switching the input language.

For example, under an existing physical-device entry:

```toml
disabled_keys = ["caps_lock", "windows_command"]
```

The other accepted names are `num_lock`, `scroll_lock`, and `insert`. Removing
the field stops managing this setting; `disabled_keys = []` instead requests
that every supported known key be enabled. OpenLogi preserves advertised
disabled bits it does not recognise, rather than clearing firmware settings
outside these five controls.

Controls require an online device with a persistent identity, a writable
configuration, and a successful read. A change is written to the keyboard and
read back before the app saves the confirmed known keys and asks the agent to
reload. An error or timeout is not proof that the keyboard stayed unchanged:
the app discards its old value, reads again, and prevents further edits until
that read succeeds. **Retry** retries an exhausted read. An unconfirmed request
is never saved as the device's setting. **Save Retry** and **Reload Retry**
recover their respective persistence steps without repeating the hardware
write. Save Retry rereads the configuration to preserve external edits and
refreshes the app's settings and binding displays from that revision, even if
the subsequent save must be retried again.

Reload acceptance is tied to the saved file revision of each change, even when
several requests share one agent reload. Acceptance of a newer save does not
confirm an older one. If the file changed externally, the app refreshes it
without overwriting it, requests acceptance of that exact revision, and then
reads the keyboard again before editing resumes. This also covers another edit
between the agent's reload and the app's refresh. It applies to every keyboard,
including when Save Retry or a save of another setting finds the external edit.
A failed refresh or agent reload keeps editing blocked; **Reload Retry** resumes
this reconciliation. A device read alone cannot
resolve an unconfirmed file revision.

Recovery stays inside the device card, using the same status and retry controls
as other device settings. Messages follow the selected interface language;
technical read, write, and configuration errors are recorded in the app logs.
Switches and recovery actions retain identity per physical device, not per
translated label or connection route. Each switch's accessible name states the
disable action and the key.

The agent reapplies a managed device's configured policy on initial discovery,
reconnect, and wake, with bounded delayed confirmation attempts. If an
unconfirmed manual write may have changed hardware, it schedules restoration
of the latest confirmed choice (including a choice not yet saved by the app),
without overtaking a newer pending request. Once a restoring write is queued,
the agent moves the keyboard's published settings revision, so an open app reads
the keyboard again after that write. New configuration or a retired
connection invalidates obsolete work. Failed inventory scans do not count as
successful confirmation: they use bounded delayed retries, retaining pending
confirmation for the normal recovery scan after those retries are exhausted.
An unreachable keyboard cannot be guaranteed to accept a setting; reconnect
or wake starts a new lifecycle attempt.

Manual writes retain their setting turn, receiver lease, and transport turn
until each submitted native report completes, including discovery and readback
reports. Cancellation is checked before submitting the next report. A completed
write becomes the recovery baseline only when the requester consumes its
successful result; an unread result is still unconfirmed. Reads of this setting
queue behind already scheduled policy writes, including workers that have not
started running yet. Ordinary standalone reads still use bounded query timeouts.
A native driver operation that never completes can therefore keep subsequent
writes waiting; a requester timeout does not make it safe to release that turn.
A request cancelled before its first report leaves the queue at once and
releases the turns and lease it was waiting for.

When a receiver slot or a directly connected keyboard reports a different known
unit ID or serial, the agent retires the former device's requests and starts a
fresh discovery retry budget. Learning previously missing identity information
for the same slot, or a serial for a direct keyboard with the same nonzero unit
ID, instead preserves active work and its remaining budget. Replacement cannot
be detected from a slot alone when neither snapshot provides distinguishing
identity, and a direct keyboard without a nonzero unit ID counts as new when its
identity changes.

### Application aliases

`[apps.<alias>]` names one application on every platform, so one per-app
profile can follow it from macOS to Linux and Windows. An alias name uses only
ASCII letters, digits, `-`, and `_`. Its fields are:

- `name` (optional): what the GUI calls the application. Without it, the GUI
  uses the name the agent reports for the application, then the alias name.
- `macos`, `linux`, and `windows` (each optional): the identifiers that
  platform reports for the application, as one string or an array. They take
  the same forms as `per_app_bindings` keys. Windows identifiers are
  lower-case, and `exe:<filename>.exe` matches every install path.

```toml
[apps.chrome]
name = "Google Chrome"
macos = "com.google.Chrome"
linux = ["google-chrome", "Google-chrome"]
windows = "exe:chrome.exe"

[devices."<physical-key>".per_app_bindings."app:chrome"]
Back = "PrevTab"
```

A profile keyed `app:<alias>` in `per_app_bindings` or `action_ring.per_app`
applies when the application in front has one of the alias's identifiers for
the running platform. On a platform the alias lists no identifier for, the
profile stays in the file and does not apply.

The most specific key wins, in this order:

1. the exact identifier
2. the Windows `exe:<filename>` key
3. the alias that lists the exact identifier
4. the alias that lists `exe:<filename>`

A key in the platform's own namespace therefore wins over an alias profile for
the same application.

The config stops loading when an `app:` key names an alias that `[apps]` does
not define. It also stops loading when two aliases list the same identifier
for one platform, or when an identifier is empty, starts with `app:`, or is a
Windows identifier with an upper-case letter.

The desktop app shows an alias profile under the alias's name and the icon of
its first identifier on this platform. It does not offer an application in
Recent applications or All applications when that application already runs
under a profile. The desktop app does not create aliases; write them here.

## Actions

Action names are the serialized Rust variant names, including `Copy`,
`BrowserBack`, `PlayPause`, `CycleDpiPresets`, and `ShowActionsRing`.

`TogglePrecisionMode` switches a mouse between its saved speed and its
slowest speed, as in Logi Options+:

| Mouse | Slowest speed | Saved speed | When none is saved |
| --- | --- | --- | --- |
| Has `0x2205` and no adjustable DPI, such as the MX Ergo | 0.125× | `pointer_speed` | 1× |
| Has adjustable DPI (`0x2201` / `0x2202`) | The lowest DPI the sensor lists | `dpi` | The sensor's default DPI |

`CycleDpiPresets` also switches precision mode on a `0x2205` mouse, because
that mouse has no adjustable DPI; DPI presets saved for it are ignored. This
makes the MX Ergo's precision button work with its default binding. On a mouse
with adjustable DPI, `CycleDpiPresets` keeps cycling the DPI presets.

A mouse with a DPI LED (HID++ `0x1300`), such as the MX Ergo, shows the mode
on that LED, as in Logi Options+. The LED lights when precision mode starts
and goes dark when it ends. OpenLogi takes control of the LED from the
firmware the first time precision mode starts, and keeps it. A mouse that
never uses precision mode keeps the LED behavior of its firmware.

Precision mode stays on when the mouse reconnects. It ends at the next press,
at a speed or DPI change from the desktop app or `config.toml`, and at a
binding change for that mouse. On a mouse with adjustable DPI, a DPI preset
action also ends it. A change made while the mouse is offline also ends it,
and the mouse reconnects at its saved speed, or at the speed for none saved.
After an agent restart, precision mode is off. A mouse with a saved speed gets
that speed when the agent connects to it. A mouse without one keeps the
slowest speed until precision mode is switched on and off again.
Payload actions use a one-key inline table:

```toml
Back = { CustomShortcut = "Cmd+Shift+P" }
Forward = { HoldShortcut = "Ctrl+Space" }
GestureButton = { TapKeyHoldingModifiers = "Cmd+Tab" }
MiddleClick = { OpenApplication = { path = "~/Downloads", display_name = "Downloads" } }
DpiToggle = { short = "ShowDesktop", long = "MissionControl" }
```

Chord modifiers are `Cmd`, `Ctrl`, `Alt`, `Shift`, and `Super`. `Cmd` is the
cross-platform primary modifier: Command on macOS, Control on Linux and Windows,
so `Cmd+C` copies everywhere. `Super` (aliases `Win`, `Meta`) always presses the
platform's logo key — Command on macOS, the Windows key, `KEY_LEFTMETA` on Linux —
so `Win+L` locks Windows and `Super+End` reaches GNOME and KDE shortcuts bound to
Super.

A chord has exactly one other key, and it may stand alone (`PageUp` with no
modifier): a letter, a digit, `` - = [ ] \ ; ' ` , . / ``, `F1` to `F20`,
`Enter`, `Escape` (`Esc`), `Backspace`, `Tab`, `Space`, the arrows `Left`,
`Right`, `Up`, `Down`, and the editing keys `Insert` (`Ins`), `Delete` (`Del`),
`Home`, `End`, `PageUp` (`PgUp`), and `PageDown` (`PgDn`). Names are
case-insensitive. On macOS, `Insert` presses the Help key.

`CustomShortcut` emits an immediate key-down/key-up pair. `HoldShortcut` keeps
the chord down until the originating physical button is released, and also
releases it if capture is interrupted, the binding becomes invalid, or the
agent shuts down. Use it for push-to-talk and other hold-to-activate controls.

`TapKeyHoldingModifiers` splits the chord: the modifiers are held for the same
lifetime as `HoldShortcut`, while the ordinary key is tapped exactly once when
the press starts. That is what an application switcher needs. `Cmd+Tab` steps
one window forward and leaves the switcher on screen until the button is
released, where `HoldShortcut` would hold Tab down too and let key repeat walk
the switcher to its last window, and `CustomShortcut` would release Cmd at once
and dismiss it. The same shape covers ``Cmd+` ``, `Alt+Tab`, and an IDE's
`Ctrl+Tab`. A chord with no modifier degrades to a plain tap of its key. If
another button is already holding the same key through `HoldShortcut`, the tap
is skipped and only the modifiers go down, so that button keeps its key held.

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

A wheel-hold binding on `Back` or `Forward` turns the main scroll wheel into a
virtual thumb wheel while the button is held, as Logi Options+ offers on mice
without a thumb wheel such as the MX Anywhere family:

```toml
Back = { click = "BrowserBack", wheel = "HorizontalScroll" }
```

Turning the wheel away from you scrolls left and toward you scrolls right,
reversed when the device's `invert_scroll` is on. Releasing the button without
turning the wheel fires `click`; a press that scrolled fires nothing else. It is
off unless you bind it.

The held scroll needs raw-wheel reporting on that button, which only some
firmware offers, and the mouse hook, so OpenLogi can post the scroll (on macOS,
the Accessibility permission). Without either, OpenLogi captures the button for
`click` alone, as it does a long-press pair, and the wheel scrolls as usual.
With mouse capture turned off the button keeps its native function, like every
other mouse binding. While the button is diverted the wheel scrolls only through
OpenLogi, so OpenLogi hands both back to the firmware when the binding changes,
the device disconnects, or the agent exits. It never sets the firmware's
persistent flag, so the diversion does not outlive a device power cycle.

In the desktop app, select `Back` or `Forward` in the default profile and turn
on **Hold to scroll sideways**. The switch appears only on mice that report raw
wheel on that button; a binding carried over to another mouse still shows it,
so it can be turned off. Picking a different action for the button keeps the
wheel hold and changes `click`. A button in gesture mode offers the switch too:
turning it on leaves gesture mode with the gesture map's `Click` as the press,
and turning gestures back on restores the saved directions. The setting is per
device, not per application: an app profile that gives the button its own
action replaces the wheel hold in that application.

An Actions Ring entry wraps the action and may add an icon or literal label:

```toml
Top = { action = { CustomShortcut = "Cmd+Shift+P" }, icon = "Keyboard", label = "Command Palette" }
```

`ShowActionsRing` is rejected inside a ring slot to prevent recursive rings.
