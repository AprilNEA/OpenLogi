# Usage (CLI)

The `openlogi` command-line tool. For install and configuration, see the
[README](../README.md).

```sh
openlogi list                 # paired devices: slot, codename, kind, online, battery
openlogi assets sync          # pre-fetch device renders from the fastest available mirror
openlogi diag features        # dump every HID++ feature the active device reports
openlogi diag controls        # dump reprogrammable controls and capability flags
openlogi diag dpi             # read → write → read-back → restore DPI (smoke test)
openlogi diag smartshift      # toggle SmartShift and restore (smoke test)
openlogi diag lighting ff0000 # solid colour for a wired RGB keyboard (any RRGGBB hex)
```

Running `openlogi` with no subcommand defaults to `list`. Set
`OPENLOGI_LOG=debug` for verbose tracing in the CLI, GUI, or agent.

## Agent-backed settings for scripts and integrations

`control` uses a compatible, already-running OpenLogi agent. It never falls back
to direct HID access, launches an agent, or edits saved profiles. The agent owns
device access and the platform permissions it needs.

```sh
openlogi control devices
openlogi control dpi --device "MX Master 3S"
openlogi control dpi --device "MX Master 3S" --set 1600
openlogi control smartshift --device "MX Master 3S"
openlogi control smartshift --device "MX Master 3S" --mode ratchet --threshold 20
```

Each successful command emits one JSON document on stdout with
`schema_version: 1`. Failures exit nonzero and describe the error on stderr.
`devices` includes `agent_version`, `protocol_version`, `inventory_health`, and
an array of addressable HID++ devices with `id`, `route`, `name`, `online`,
`battery`, and `capabilities`. Missing battery or capabilities are `null`;
inventory health distinguishes a ready inventory from one still scanning or
unavailable. Standalone cameras and lighting devices are not included.

Use an exact full device name (case-insensitive) or an exact `id` returned by
`devices`. Ambiguous names or routes fail, including two direct devices whose
vendor/product IDs cannot distinguish them. IDs identify current routes; they
are not a promise of persistent identity across connection changes. Per-device
commands require a ready inventory and an online target.

Without a write option, `dpi` and `smartshift` only read and return
`written: false` and `current`. DPI reports the current value and advertised
capabilities. SmartShift reports wheel mode, automatic disengage threshold, and
tunable torque when supported. `--mode` accepts `free` or `ratchet`;
`--threshold` accepts 1–255, where 255 means permanent ratchet. Omitted
SmartShift fields, including torque, retain their existing values.

Writes validate supported DPI, issue one request, then read back the setting.
Only a matching read-back returns `written: true`, `before`, `current`, and
`persistence: "temporary"`. Saved profiles or a reconnect can replace these
temporary settings. A timeout, disconnect, or mismatched read-back fails without
an automatic retry; a write may already have taken effect. Query the setting
before deciding whether to retry.

## Device assets

Asset synchronization probes `assets.openlogi.org`, the versioned Cloudflare
Pages release alias, and the pinned jsDelivr npm release concurrently. The first
mirror with a valid catalog supplies every file for that synchronization run.
Set `OPENLOGI_ASSETS` or pass `openlogi assets sync --base <URL>` to use one
uniform asset origin instead of automatic mirror selection.
