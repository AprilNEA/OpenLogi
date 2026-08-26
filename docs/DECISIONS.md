# Decision log

Durable "why we did it this way" records that are not obvious from the code.
Add a dated entry when a non-obvious architectural or dependency decision is
made or revisited.

## 2026-08: Device settings are keyed by identity, not by transport

A device's config key was derived from how it was reached, so the same mouse
on a receiver and on a cable became two devices: two entries, two carousel
cards, settings left behind on whichever link set them. The correlating value
was already read on both paths and thrown away — `DeviceStableId::from_parts`
took `serial` and `unit_id` and discarded both for receiver routes.

- Keys are now `unit:<hex>` or `serial:<s>`, with a persisted `links` table
  naming the routes the device has been seen on. That table is also the index
  that identifies a sleeping device when only its route is known, which is why
  it is stored rather than recomputed. It fixes a second bug in passing:
  unpairing mouse A from a slot and pairing B into it no longer hands B all of
  A's bindings, because the key is no longer the slot. Not retroactively,
  though — a pre-upgrade config records nothing about which device wrote a slot
  entry, so the first sighting after the upgrade still folds it into whatever
  now occupies the slot. Only from that point on is the entry keyed by unit and
  the slot empty.
- A device whose unit id reads all-zero keeps its route key and never
  correlates — degradation, not an assumption about hardware we cannot sample.
- Capabilities are measured per link, because they genuinely differ per link:
  a G502 LIGHTSPEED publishes `0x2121 HiResWheel` over its receiver and
  `0x00c2 DfuControlSigned` over USB, same firmware image either way (#660).
  The probe was never wrong; one device simply could not own both readings.
  Each link's capabilities are rewritten from the sighting that reached it, so
  the table describes the hardware rather than whatever a migration happened to
  leave behind. Settings that disagree between links become per-link overrides
  rather than one link overwriting the other.
- Migration is two-phase because a v4 direct key carries the unit id in the key
  string while a receiver key says nothing about the device: schema 4 → 5
  renames direct keys mechanically at load, and receiver entries fold into
  their canonical entry on the next online sighting. Until that sighting the
  legacy entry stays orphaned. Closing that window at load time by matching
  entries on identical `model_ids` was rejected — model ids are model-scoped,
  so two identical mice, one on a receiver and one cabled, would merge into one
  device. That is the property slot-keying protected, and it must not regress.

## 2026-08: Suppressions are `expect`, and tests are exempt by config

A sweep of every lint suppression in the tree (207 attributes, 247 lints) found
two systemic problems, both now fixed.

- **Tests restated the same exemption 78 times.** `unwrap_used`/`expect_used`
  stay at warn so product code has to state its panics, but every test module
  had copied `#[allow(…, reason = "idiomatic in tests")]` to opt out — about
  40% of all suppressions in the workspace, carrying no information. A root
  `clippy.toml` with `allow-unwrap-in-tests` / `allow-expect-in-tests` replaces
  all of them. Clippy's exemption covers `#[cfg(test)]` modules and `#[test]`
  functions; a free helper in a `tests/` integration file is the one shape it
  cannot see, so `openlogi-ipc`'s wire-format test keeps a file-level
  suppression. Build scripts are not tests and keep theirs too.
- **`allow` rots silently.** 20 suppressions no longer suppressed anything,
  including three module-wide `dead_code` blankets in `openlogi-assets` that
  had been inert since those modules became `pub mod` — and would have hidden
  real dead code the moment they went private again. Suppressions are now
  `#[expect]` by default, which fails the build once it stops being needed.
  `allow` survives only where `expect` cannot work: a lint that fires under
  some `cfg` but not others, one whose fulfilment differs between a crate's
  lib and test targets, and one raised inside a macro expansion (rustc does not
  credit an expectation with those, so it both suppresses the warning and
  reports itself unfulfilled). Each such site carries a comment saying which.
- **Both rules are now machine-checked.** `allow_attributes` and
  `allow_attributes_without_reason` join the lint table, costing three
  annotated exceptions and nothing else — the sweep had already left the tree
  compliant. One blind spot to remember: `allow_attributes` only sees outer
  `#[allow]`, so a module-wide `#![allow(…)]` — precisely the shape that rotted
  in `openlogi-assets` — still passes it. Adding them also disproved the
  first-pass rule that a `cfg_attr`-wrapped suppression always needs `allow`:
  two of the four turned out to work fine as `expect`.

Related: the seven file-wide `cast_*` blankets were narrowed to the functions
that need them, and most of their sites turned out not to need a suppression at
all — `cast_signed`/`cast_unsigned`, `&raw const`/`&raw mut`, `to_le_bytes`, or
a shared conversion helper. Linux CI showed `capture_linux`'s file-level
blanket was two-thirds dead (`cast_possible_truncation` /
`cast_possible_wrap`); the remaining `cast_sign_loss` sits on `clamp_u8`.

Supersedes the "`openlogi-hidpp` stays out on purpose (vendored)" note in the
shared-lint-set entry below: the hard-fork ruling retired that, and the crate
inherits `[lints] workspace = true` like every other.

Sweeping for rot is mechanical and worth repeating: rewrite every
non-`cfg_attr` `allow(` to `expect(`, run clippy, and each "lint expectation is
unfulfilled" is a suppression to delete. Run it on all three lanes — native,
`--target x86_64-pc-windows-gnu`, `--target aarch64-unknown-linux-musl` —
because CI has no macOS clippy job and a platform-gated suppression is only
evaluated on its own platform.

## 2026-08: Shared clippy lint set

The workspace adopted the shared ten-lint set (`assertions_on_result_states`,
`cast_possible_truncation`, `cast_possible_wrap`, `cast_sign_loss`,
`error_impl_error`, `exit`, `or_fun_call`, `ptr_as_ptr`,
`tests_outside_test_module`, `undocumented_unsafe_blocks`) on top of the
existing `pedantic` + `unwrap_used`/`expect_used` table.

- One table, inherited everywhere. `openlogi-desktop`, `openlogi-camera` and
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

## 2026-06: Fn is not a capturable trigger (macOS)

An instrumented `CGEventTap` probe during the function-key remapper design
(the spec lived in `docs/superpowers/` until 2026-08; full text at
[91fe5d80](https://github.com/AprilNEA/OpenLogi/blob/91fe5d80f3a2c16cf16061b3abb5a01d47fc8637/docs/superpowers/specs/2026-06-30-function-key-remapper-design.md))
settled why the Fn modifier cannot join the keyboard-remap trigger vocabulary:

- F1 arrives as keycode 122 **with** the `SecondaryFn` flag, but plain `Q` and
  `Fn+Q` are byte-for-byte identical, as are plain Shift and `Fn+Shift`;
  pressing Fn alone produces no event at all (not even `FlagsChanged`).
- The keyboard firmware holds Fn internal unless the key has a dual
  function-row meaning, so the flag attaches only to F1–F12.
  `Fn+<anything else>` is indistinguishable at the tap — firmware behavior,
  not something OpenLogi can code around at this layer.
- The only theoretical path is raw-HID reading below the OS event system
  (Karabiner/DriverKit territory) — a large subsystem with no guarantee a
  given keyboard exposes Fn there. Not pursued.
