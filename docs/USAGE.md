# Usage (CLI)

The `openlogi` command-line tool. For install and configuration, see the
[README](../README.md).

```sh
openlogi list                 # paired devices: slot, codename, kind, online, battery
openlogi assets sync          # pre-fetch device renders from the fastest available mirror
openlogi diag features        # dump every HID++ feature the active device reports
openlogi diag controls        # dump reprogrammable controls and capability flags
openlogi diag dpi             # read → write → read-back → restore DPI (smoke test)
openlogi diag smartshift                # toggle SmartShift and restore (smoke test)
openlogi diag smartshift --torque 60    # set scrolling force / feedback intensity (1-100%)
openlogi diag lighting ff0000           # solid colour for a wired RGB keyboard (any RRGGBB hex)
```

Running `openlogi` with no subcommand defaults to `list`. Set
`OPENLOGI_LOG=debug` for verbose tracing in the CLI, GUI, or agent.

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
| `smartshift` | `device`, `persistence`, `mode` (`free` or `ratchet`), `auto_disengage` (integer), `tunable_torque` (integer or null) |
| `fn-lock` | `device`, `persistence`, `fn_lock`, `default_fn_lock` |

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
`level`, and `status` (for example `charging` or `discharging`). Capabilities
are the agent's measured feature flags, not guesses based on device kind.
Not every readable setting has an inventory flag; a setting read can return
`unsupported_feature`. Standalone lights are listed with their light capability
descriptor and null battery. Cameras are not part of this agent inventory API.
The response excludes pairing passkeys, application history, and model serials.

### Immediate control and saved preferences

All three setting commands read only unless a setting flag is supplied. Without
`--save`, writes return `persistence: "not_saved"`: they do not modify
`config.toml` or request a config reload. Saved preferences may be reapplied on
reconnect, wake, or later configuration changes; firmware may retain some values.

Add `--save` to an explicit DPI, SmartShift, or Fn-lock change to save the verified
setting and request an agent config reload. Success returns `persistence: "saved"`.
The device must have a probed physical identity. Existing legacy configuration
entries are respected; CLI route IDs are never used directly as config keys.
Saving edits only the selected setting, preserving unrelated preferences and
comments. A per-link override of that setting is refused with `link_override`
rather than silently saving a default that the override would supersede.
There is no profile-switch API yet.

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

Asset synchronization probes `assets.openlogi.org`, the versioned Cloudflare
Pages release alias, and the pinned jsDelivr npm release concurrently. The first
mirror with a valid catalog supplies every file for that synchronization run.
Set `OPENLOGI_ASSETS` or pass `openlogi assets sync --base <URL>` to use one
uniform asset origin instead of automatic mirror selection.
