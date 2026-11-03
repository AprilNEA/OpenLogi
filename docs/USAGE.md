# Usage (CLI)

The `openlogi` command-line tool. For install and configuration, see the
[README](../README.md).

For the desktop app's Add Device flow, see [wireless pairing](PAIRING.md).

```sh
openlogi list                 # paired devices: slot, codename, kind, online, battery
openlogi assets sync          # pre-fetch device renders from the fastest available mirror
openlogi diag features        # dump every HID++ feature the active device reports
openlogi diag controls        # dump reprogrammable controls and capability flags
openlogi diag mouse-buttons   # print the 0x8110 slot each G-series mouse button sends
openlogi diag dpi             # read → write → read-back → restore DPI (smoke test)
openlogi diag pointer-speed             # test 0x2205 pointer scaling and attempt to restore it
openlogi diag smartshift                # toggle SmartShift and restore (smoke test)
openlogi diag smartshift --torque 60    # set scrolling force / feedback intensity (1-100%)
openlogi diag lighting ff0000           # solid colour for a wired RGB keyboard (any RRGGBB hex)
openlogi diag host-switch --device "MX Master 3S" --host 1   # move a device to Easy-Switch channel 2
```

Running `openlogi` with no subcommand defaults to `list`. Set
`OPENLOGI_LOG=debug` for verbose tracing in the CLI, GUI, or agent.

`openlogi diag pointer-speed` temporarily changes the selected device's pointer scaling. The command attempts to restore and verify the original scaling even when the test write fails. `--device NAME` selects a device; `--target RAW` sets the temporary 8.8 fixed-point value (`256` is 1×). By default, the command halves the current value, or doubles it when the raw value is `1`. Firmware may accept extreme values without clipping. A restoration error means the original speed could not be confirmed.

`openlogi diag mouse-buttons` starts the HID++ `0x8110` button spy of a G-series mouse and prints the held slots on every press and release, with the G number G HUB shows for each slot. This is the slot order a new model's button layout needs. In onboard mode the active profile drives the buttons, and a button it leaves unbound may report nothing, so the command puts a mouse with `0x8100` in host mode for the watch, where every slot reports and each slot sends the HID button the printed mapping gives it (`0` sends nothing). It listens for `--duration SECS` (default 60), then stops the spy and restores onboard mode. Ctrl-C skips the restore; switch the mouse off and on to leave host mode. The command refuses to run while the OpenLogi agent runs, because the agent arms host mode itself. It also refuses a mouse already in host mode, or one whose mode it cannot read, because G HUB or another program owns the spy then. A mouse left in host mode by an interrupted watch, or by a program that quit without restoring it, leaves host mode only when it is switched off and on.

`openlogi diag host-switch` asks the running agent to move one device to another Easy-Switch channel through HID++ `0x1814`. `--host` is zero-based, so `--host 1` is channel 2. `--device NAME` must match exactly one online device; a unique exact name wins over longer matches. The agent refuses a channel the device does not have and a channel the device reports as unpaired, before anything is written. Success means the agent sent the switch. It does not confirm that the device arrived, because the device then leaves this computer's radio link.

## Keyboard backlight

`openlogi backlight` reads and changes the white HID++ Backlight2 (`0x1982`)
backlight, for example on MX Keys. It requires a running, protocol-compatible
OpenLogi agent. Start OpenLogi normally and use the same `OPENLOGI_PROFILE` for
the app and CLI. The command uses the agent's inventory and existing channel;
it never opens HID directly, starts the agent, or falls back to diagnostics.
It does not arm a dormant agent's input stack.

```sh
openlogi backlight                         # status, no write
openlogi backlight --device "MX Keys" status
openlogi backlight --device "MX Keys" off
openlogi backlight --device "MX Keys" on
openlogi backlight --device "MX Keys" auto on
openlogi backlight --device "MX Keys" level 3
openlogi backlight --device "MX Keys" battery-saving on
```

`--device NAME` is a case-insensitive name substring, not the automation API's
opaque route ID. It must match exactly one online HID++ device. Without it,
automatic selection requires exactly one online device with measured backlight
support. Unknown capabilities require an explicit name and an agent read;
known unsupported devices are refused. An incomplete inventory, missing route,
or ambiguous selection fails before any backlight operation. Identical direct
devices sharing a route cannot be disambiguated by name.

**Compatibility change:** earlier versions could open hardware without an agent
and choose the first matching device. This command now fails instead. This does
not change `list` fallback, hardware diagnostics, or the JSON API's selection.

Bare invocation and `status` only read. Changes are saved in the keyboard's
non-volatile memory, not `config.toml`, and survive reconnects, host switches,
and power cycles. The agent confirms writes; the CLI prints that returned state
as `read-back` and only then reports success. `status` lists which additional
controls the firmware offers. The agent rejects unsupported modes or levels.
This command does not add timer or effect settings.

With the matching agent and GUI build, an already-open Backlight panel refreshes
after a CLI write; reopening the device or restarting the app is unnecessary.
The agent announces a device-settings revision and the GUI reads the keyboard
again. A failed write can also trigger a refresh because the keyboard may have
changed before confirmation failed; the refresh is not a success report.

Failures exit nonzero. If the agent times out or disconnects during a write,
the outcome is **unconfirmed**: the keyboard may already have applied it.
The command does not automatically retry. Reconnect to the matching agent and
read `status` before deciding whether another write is needed. Firmware's
temporary-manual mode cannot be restored by software; the command warns when
an otherwise mode-preserving change would return it to automatic mode.

## Automation API

`openlogi api` is the JSON interface for launchers such as Raycast and local
scripts. It requires a running, protocol-compatible OpenLogi agent. It never
opens hardware directly, starts the agent, or falls back to diagnostic commands.
Start OpenLogi normally before using it. Existing human-readable commands are
unchanged.

```sh
openlogi api status
openlogi api devices
# Copy an exact, non-null id from api devices; quote it as one argument.
openlogi api dpi --device "$DEVICE_ID"
openlogi api dpi --device "$DEVICE_ID" --set 1200
openlogi api dpi --device "$DEVICE_ID" --set 1200 --save
openlogi api smartshift --device "$DEVICE_ID"
openlogi api smartshift --device "$DEVICE_ID" --mode free
openlogi api smartshift --device "$DEVICE_ID" --mode ratchet --auto-disengage 255
openlogi api fn-lock --device "$DEVICE_ID"
openlogi api fn-lock --device "$DEVICE_ID" --set on
openlogi api os-layout --device "$DEVICE_ID" --set off --save
openlogi api report-rate --device "$DEVICE_ID"
openlogi api report-rate --device "$DEVICE_ID" --set 1000 --save
openlogi api easy-switch --device "$DEVICE_ID"
openlogi api easy-switch --device "$DEVICE_ID" --channel 2 --name "Work laptop"
```

### Output and compatibility

After arguments parse, stdout contains exactly one JSON object and a newline.
Success exits with status **0**; runtime failure exits with status **1**:

```json
{"schema_version":1,"ok":true,"data":{"inventory":"ready","devices":[]}}
```

```json
{"schema_version":1,"ok":false,"error":{"code":"agent_unavailable","message":"…"}}
```

Consumers must check `schema_version` and `ok`, ignore unknown object fields,
and handle unknown error codes as failures. Breaking changes to this JSON
contract require a schema version change; it is not the internal IPC version.
Error messages are explanatory text, not stable values to parse. CLI syntax
errors retain standard clap behavior (exit **2**, diagnostics on stderr);
`--help` and `--version` are text. Logging also goes to stderr. Failure to write
stdout cannot provide a JSON envelope.

| Command | `data` fields |
| --- | --- |
| `status` | `agent_version`, `inventory`, `accessibility_granted`, `input_monitoring_granted`, `hook_installed`, `hid_open_failures`, `launch_at_login` |
| `devices` | `inventory`, `devices`: array of `id`, `name`, `kind`, `online`, `battery`, `capabilities`, `light_capabilities` |
| `dpi` | `device`, `persistence`, `current` (integer), `supported` (array of integers) |
| `smartshift` | `device`, `persistence`, `mode` (`free` or `ratchet`), `auto_disengage` (integer), `tunable_torque` (1–100, percent of the wheel's maximum force, or null) |
| `fn-lock` | `device`, `persistence`, `fn_lock`, `default_fn_lock` (boolean, or null when the keyboard reports no firmware default) |
| `os-layout` | `device`, `persistence`, `keep_os_layout` (boolean) |
| `report-rate` | `device`, `persistence`, `current` (integer, hertz), `supported` (array of integers in hertz, for the current connection) |
| `easy-switch` | `device`, `persistence`, `current_channel` (integer from 1, or null), `can_rename` (boolean), `channels`: array of `channel` (integer from 1), `paired` (boolean), `connection` (`unknown`, `unifying`, `usb`, `bluetooth`, or `bolt`), `name` (string or null), `name_max_len` (integer, UTF-8 bytes), `os` (string such as `macOS 15.2`, or null) |

`inventory` is `scanning`, `ready`, or `unavailable`. Only `ready` with an empty
device array means enumeration completed and found no peripherals. Device
operations are refused until inventory is ready. Status queries do not arm a
dormant agent's input stack; opening the desktop app follows the normal lifecycle.

Device IDs are opaque current-route identifiers, not persistent physical-device
keys. Refresh them after reconnects or updates; do not construct IDs, parse them,
or use array positions or names to address devices. Missing routes have a null
ID. Duplicate IDs are returned without deduplication, and operations against
them fail with `ambiguous_device`. In particular, the current transport cannot
distinguish identical direct-attached devices that share one route. IDs can
contain hardware identity information: keep them local and redact before sharing.

`name`, `battery`, and measured `capabilities` may be null; null does not mean
zero charge or an unsupported feature. Battery objects contain `percentage`,
`level`, and `status` (for example `charging` or `discharging`). `percentage`
is null when the device has no state-of-charge gauge and reports only a level,
or when the reading carries no charge figure; read `level` in that case.
Capabilities are the agent's measured feature flags, not guesses based on
device kind.
Not every readable setting has an inventory flag; a setting read can return
`unsupported_feature`. Standalone lights are listed with their light capability
descriptor and null battery. Cameras are not part of this agent inventory API.
The response excludes pairing passkeys, application history, and model serials.

### Immediate control and saved preferences

The DPI, SmartShift, Fn-lock, and report-rate commands read only unless a setting flag is supplied. Without
`--save`, writes return `persistence: "not_saved"`: they do not modify
`config.toml` or request a config reload. Saved preferences may be reapplied on
reconnect, wake, or later configuration changes; firmware may retain some values.

Add `--save` to an explicit DPI, SmartShift, Fn-lock, or report-rate change to save the verified
setting and request an agent config reload. Success returns `persistence: "saved"`.
The device must have a probed physical identity. Existing legacy configuration
entries are respected; CLI route IDs are never used directly as config keys.
Saving edits only the selected setting, preserving unrelated preferences and
comments. A per-link override of that setting is refused with `link_override`
rather than silently saving a default that the override would supersede.
There is no profile-switch API yet.

`os-layout` changes an agent setting rather than the device, so it requires
both `--set` and `--save`; without `--save` it fails with `invalid_value`. The
agent applies the saved value after the reload. `on`, the default for keyboards
that allow it, sets the keyboard's key layout (HID++ `0x4531`) to the host OS
when the keyboard connects, when the setting turns on, and after the keyboard
reports that a layout shortcut switched it. `off` leaves the keyboard's layout
as it is; it does not restore an earlier layout. A keyboard whose measured
capabilities lack `os_layout` returns `unsupported_feature`.

`report-rate --set` accepts only a rate in `supported`. That list belongs to
the device's current connection — a gaming mouse can offer more rates over its
receiver or a cable than over Bluetooth — so any other value fails with
`invalid_value` before anything is written. A saved rate is re-applied on
reconnect only over a connection that lists it. A device without HID++
`0x8060` or `0x8061` returns `unsupported_feature`.

`easy-switch` lists the device's Easy-Switch channels (HID++ `0x1815`) and the
name and OS each paired computer reported. `--channel` and `--name` together
rename the computer on that channel; the device stores the name itself, so
there is no `--save` (it fails with `invalid_value`) and the result is always
`persistence: "not_saved"`. Surrounding whitespace is trimmed. A name that is
empty, contains NUL, or is longer than the channel's `name_max_len` bytes, or a
channel with no paired computer, fails with `invalid_value` before anything is
written. A device that does not let software rename hosts (`can_rename` is
false) returns `unsupported_feature`, and a name the device does not read back
returns `readback_mismatch`. A device without `0x1815` returns
`unsupported_feature`.

The desktop app shows the same channels in an Easy-Switch card on the Device
tab and renames a paired channel under the same name rules. Neither the CLI
nor the app renames a channel on its own: unlike Logi Options+, OpenLogi does
not write this computer's name or OS onto the current channel when it starts.

Hardware writes, file persistence, and reload are separate steps, not an atomic
transaction. The config revision is checked under a writer lock; concurrent edits
are not overwritten. A lock conflict is reported immediately, without waiting.
The persistent `config.toml.lock` sidecar must not be deleted while writers run.
External editors that do not honor this lock can still race with a save.

If saving fails after verified hardware readback, the JSON error includes
`persistence: "not_saved"` with `config_conflict`, `config_busy`, or
`config_save_failed`. The live hardware has already changed; no rollback or
automatic retry is attempted. If saving succeeds but reload fails, the error
includes `persistence: "saved"`: `config_reload_failed` means rejection;
`config_reload_unknown` means timeout/disconnection with an unknown reload
outcome. Neither is reported as success. Other errors occur before persistence;
hardware outcomes after a failed write/readback may still be uncertain.

DPI must be in the device-reported supported list; values are never silently
rounded. SmartShift preserves unspecified fields, including wheel torque.
`auto_disengage` is 1–254 in firmware units of 0.25 turn/s; 255 means permanent
ratchet, and 0 is rejected. `--mode ratchet` alone does not disable automatic
release: use `--auto-disengage 255` as well. Fn lock `on` means bare function keys
send F1–F12; `off` means the printed media functions.
With `--save`, SmartShift must use the configuration-supported range 8–255;
this also validates a threshold retained from the device when only mode changes.

DPI and SmartShift are read back after writes; Fn lock returns the firmware
echo. Reads and writes are not a transaction against simultaneous GUI changes.
A timeout, disconnect, or readback error after a write means its outcome may
be uncertain: read the setting again rather than blindly retrying the write.

Stable error codes: `agent_unavailable`, `handshake_failed`, `version_mismatch`,
`timeout`, `disconnected`, `inventory_not_ready`, `device_not_found`,
`device_offline`, `ambiguous_device`, `unsupported_feature`, `invalid_value`,
`readback_mismatch`, `device_error`, `identity_unavailable`, `link_override`,
`config_error`, `config_conflict`, `config_busy`, `config_save_failed`,
`config_reload_failed`, and `config_reload_unknown`. A protocol mismatch requires matching
OpenLogi CLI and agent builds; it never triggers direct-hardware fallback.

## Asset synchronization

Asset synchronization probes `assets.openlogi.org`, the Cloudflare Pages alias
of the `0.1` asset release line, and the newest `0.1.x` npm release on jsDelivr
concurrently. The first mirror with a valid catalog supplies every file for that
synchronization run. On jsDelivr, OpenLogi also asks `data.jsdelivr.com` for the
newest release and waits at most one second for the answer. With the answer,
jsDelivr trails a new asset release by up to about an hour; without it, by up
to 12 hours.
Set `OPENLOGI_ASSETS` or pass `openlogi assets sync --base <URL>` to use one
uniform asset origin instead of automatic mirror selection.
