# Decision log

Durable "why we did it this way" records that are not obvious from the code.
Add a dated entry when a non-obvious architectural or dependency decision is
made or revisited.

## 2026-08: Shared clippy lint set

The workspace adopted the shared ten-lint set (`assertions_on_result_states`,
`cast_possible_truncation`, `cast_possible_wrap`, `cast_sign_loss`,
`error_impl_error`, `exit`, `or_fun_call`, `ptr_as_ptr`,
`tests_outside_test_module`, `undocumented_unsafe_blocks`) on top of the
existing `pedantic` + `unwrap_used`/`expect_used` table.

- One table, inherited everywhere. `openlogi-gui`, `openlogi-camera` and
  `openlogi-hook` carried hand-copied duplicates of `[workspace.lints]`, so any
  lint added to the workspace would have silently skipped them — three of the
  crates holding most of the FFI. Cargo rejects `[lints] workspace = true`
  alongside local overrides, so `openlogi-hook` moved its `unsafe_code` opt-out
  into its three platform modules. `openlogi-hidpp` stays out on purpose
  (vendored).
- `tests_outside_test_module` only recognises a literal `#[cfg(test)]`. Compound
  gates are written as stacked attributes (`#[cfg(test)]` then `#[cfg(unix)]`);
  an integration test under `tests/` carries a file-level `#![expect(…)]`
  because that file is already a test-only crate. Splitting the attribute also
  wakes `items_after_test_module`, so such a module belongs last in its file.
- `exit` gets a real `ExitCode` wherever the call site can return — `openlogi
  list` hands status 2 back to `main` — and a reasoned `#[expect]` where it
  cannot (the AppKit run loop, the watchdog threads, the update handover).
  Clippy does not look inside `define_class!`, so the menu-bar Quit body moved
  out of the macro rather than escape the lint by accident.
- Not adopted: the policy's `unexpected_cfgs` / `check-cfg = ['cfg(kani)']`
  entry. Nothing here uses Kani, and `unexpected_cfgs` already warns by default,
  so it would be dead configuration.

## 2026-08: Standalone raw-light boundary

Standalone lights such as Litra stay outside the HID++ receiver/paired-device
model and are normalized only at the shared agent and GUI device-record
boundary. This keeps the existing HID++ wire and routing semantics unchanged
while allowing future light drivers to share capability-driven controls.

- Persist brightness as a normalized percentage and temperature as Kelvin;
  native units and report encoding remain driver responsibilities.
- Use device serials for persistent raw-device keys. OS-node identifiers are
  runtime-only hints and must not silently become physical configuration keys.
- Advertise optional light controls through `LightCapabilities`; the GUI gates
  controls from those capabilities rather than from `DeviceKind::Light`.
- Serialize and coalesce per-device light writes in the agent so reconnect,
  camera automation, config reload, and manual commands cannot interleave at
  packet level.

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
