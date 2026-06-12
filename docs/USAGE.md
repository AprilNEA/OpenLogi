# Usage (CLI)

The `openlogi` command-line tool. For install and configuration, see the
[README](../README.md).

```sh
openlogi list                 # paired devices: slot, codename, kind, online, battery
openlogi assets sync          # pre-fetch device renders from assets.openlogi.org
openlogi diag features        # dump every HID++ feature the active device reports
openlogi diag dpi             # read → write → read-back → restore DPI (smoke test)
openlogi diag smartshift      # toggle SmartShift and restore (smoke test)
openlogi diag lighting ff0000 # solid colour for a wired RGB keyboard (any RRGGBB hex)
```

Running `openlogi` with no subcommand defaults to `list`. Set
`OPENLOGI_LOG=debug` for verbose tracing on either binary.
