---
paths:
  - "crates/openlogi-hidpp/src/feature/onboard_profiles/**"
  - "crates/openlogi-hid/src/onboard_profiles.rs"
  - "crates/openlogi-hid/src/write/onboard_profiles.rs"
  - "crates/openlogi-agent-core/src/orchestrator.rs"
  - "crates/openlogi-agent-core/src/hardware.rs"
  - "crates/openlogi-gui/src/components/profiles_panel.rs"
  - "crates/openlogi-cli/src/cmd/diag/profiles.rs"
---

# Onboard profiles — HID++ `0x8100`

Gaming mice keep profiles in their own flash and switch between them from
buttons on the device, so OpenLogi is never the only writer of the state it
shows. Protocol facts — byte layouts, what each function rejects, the
reverse-engineering sources — live in the `onboard_profiles` rustdoc; this file
carries only the constraints that cross crates.

The surface: `openlogi-hidpp/src/feature/onboard_profiles/` (protocol wrapper),
`openlogi-hid/src/onboard_profiles.rs` + `write/onboard_profiles.rs` (IPC types
and I/O verbs), `openlogi-agent-core/src/orchestrator.rs` +`hardware.rs` (policy
and sequencing), `openlogi-gui/src/components/profiles_panel.rs` (the panel),
`openlogi-cli/src/cmd/diag/profiles.rs` (the round-trip diagnostic).

The reference device is a G502 X LIGHTSPEED — the only hardware this has run
against, and the `0x8100` spec is not public. Treat single-device observations
as observations, not as the format.

- **Never change a device's mode uninvited.** `configured_onboard_profiles`
  returns `None` when the user has not chosen one, and the device keeps whatever
  mode it powered on in. Devices power on in onboard mode: the mode lives in RAM
  and does not survive a power cycle, which is exactly why a _configured_ mode is
  re-asserted on every reconnect. Never skip the reapply because an earlier
  session saw the right mode.
- **Sequence the onboard apply ahead of the other volatile writes.** Activating a
  profile reloads that profile's DPI and its other stored settings out of flash,
  so a DPI write that raced ahead would be discarded. That is what the `after`
  continuation of `apply_onboard_profiles_in_background` is for.
- **That continuation must outlive a panic.** It carries the whole rest of the
  volatile reapply, so it runs from a drop guard rather than as the spawned
  thread's last statement. Do not "simplify" it back to a trailing call.
- **Every device verb must hold `write::exchange()`.** The channel is built with
  `rotate_software_id: false`, so concurrent requests carry byte-identical HID++
  headers — and `send_v20` matches a reply by comparing headers, so two in flight
  on one device take each other's replies. The global lock is a **stopgap**: the
  root fix is `HidppChannel::set_rotating_sw_id(true)`, which needs the bench
  cases re-run on hardware. Until then, anything opening a channel outside
  `with_route` or the `*_on` fast paths must take the lock too — note
  `gesture.rs::run_capture_session` currently does not.
- **Host mode rejects `set_current_profile`** (`InvalidArgument`) and reports the
  active profile as `0x0000`, because it parks the flash profile. `diag profiles`
  enters onboard mode deliberately, not as a side effect, and skips its restore
  step when it found the device in host mode.
- **Never offer a ROM sector as a selectable profile.** The G502 X counts 2 ROM
  profiles alongside its 5 user slots, yet the sector-0 directory terminates after
  the 5 user entries and `set_current_profile` rejects `0x0101`–`0x0103`. They
  read as factory templates a slot is reset _from_, not profiles you switch _to_ —
  so the panel filters ROM entries out of the pill list and `keep_profile_for`
  never picks one, because selecting one is an action that cannot succeed. `diag
  profiles` still prints them: showing raw device state is its job. The directory
  bound stays `profile_count + profile_count_oob` — what the format allows, so a
  device that does list them is not silently truncated.
- **Never re-derive `ROM_SECTOR_FLAG` at a call site** — go through
  `openlogi_hid::is_rom_sector`.
- **`ProfilesMode` and `ProfileEntry` are wire format.** They cross the agent↔GUI
  IPC, so variant and field order changes need a `PROTOCOL_VERSION` bump and
  regenerated goldens (`.claude/rules/ipc-protocol.md`). The panel gates on
  `Capabilities::onboard_profiles`, never on device `kind`.
- **State is read once per device** (`profiles_panel.rs`, `ensure_profiles_load`);
  nothing subscribes to HID++ events, so a profile cycled on the device is
  invisible until the panel reloads. `0x8100` has no event support in the fork.
  Current state — fix it or leave it, but do not re-report it as a bug.

Verify with `openlogi diag profiles`: it reads the description and directory,
enters onboard mode, sets and restores the active profile, then restores the
original mode, on the error path too. `--read-only` skips every write;
`--leave-onboard` skips the final mode restore, which is how you stage a device
to test the agent's reapply on reconnect. Unit tests cover the parsers
(`onboard_profiles/tests.rs`); everything else here is firmware behaviour, so
real-device verification is the maintainer's job — state plainly what you did
not test rather than implying coverage.
