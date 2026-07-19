# Decision log

Durable "why we did it this way" records that are not obvious from the code.
Add a dated entry when a non-obvious architectural or dependency decision is
made or revisited.

## 2026-07: Infrastructure we keep custom instead of using a crate

A dependency audit replaced most general-purpose infrastructure code with
mature crates (`tempfile`, `which`, `plist`, `walkdir`, `xshell`, `sysinfo`,
`fs-err`, `backon`, `opener`, `etcetera`, and others — see the git history of
`FIXDRY.md` for the full list). The following stayed custom, deliberately:

- `openlogi-core::single_instance`: the `single-instance` crate uses different
  backends (for example abstract Unix sockets on Linux) and does not preserve
  OpenLogi's data-dir lock-file path, per-role names, and error classification
  closely enough to be a safe deletion.
- Agent tray Quit's `openlogi://quit` dispatch keeps
  `std::process::Command::output()` intentionally: it blocks until
  LaunchServices accepts the Apple Event, while generic opener crates only
  guarantee process spawn.
- GUI helper launch keeps `/usr/bin/open -g -n` intentionally: it needs
  LaunchServices-specific flags to start the packaged agent under its own TCC
  identity, which generic opener crates do not expose.
- Agent autostart install keeps direct `systemctl` calls because it is managing
  systemd user units, not merely opening or spawning an arbitrary program.
- Self-restart and `disclaim` launches stay custom because they are process
  identity / update lifecycle boundaries, not generic command orchestration.
- `openlogi-hook`: event suppression/rewriting and foreground-app lookup are
  OpenLogi-specific and not covered cleanly by generic input crates.
- `openlogi-inject`: platform-specific action synthesis may overlap with
  `enigo`, but current semantics are narrower and more controlled.
- `openlogi-hid` / vendored `openlogi-hidpp`: the right path is upstreaming
  OpenLogi-specific fixes, not replacing the fork blindly.
