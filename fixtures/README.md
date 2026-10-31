# OpenLogi fixture corpus

This directory is the repository-level home for reviewed, sanitized
captures produced by `openlogi fixture record`. One physical specimen owns one
directory:

```text
fixtures/devices/<synthetic-specimen-id>/
  manifest.json
  profile.json
  cases/
    <operation>.json
```

Only privacy-verified fixture assets belong here. Never commit native recorder
output, host paths, original hardware identities, passkeys, or unsanitized
temporary files. Run `openlogi fixture verify <fixture-directory>` before
review. `cargo test -p openlogi-cli fixture::verify` also discovers and strictly
verifies every fixture directory in this corpus and the packaged synthetic
corpus, including newly added specimens.

## Automatic verification

The repository tests discover every specimen through `openlogi_fixture::fs::repository_corpus`. Missing files, extra files, symlinks, invalid identities, and an absent or empty repository corpus fail the suite. Published packages without the repository workspace report that the contributed corpus is unavailable; their embedded synthetic fixture remains testable.

Run the complete recorded-data chain on macOS:

```sh
cargo test -p openlogi-cli -p openlogi-agent-core -p openlogi-agent -p openlogi-desktop contributed_corpus
cargo test -p openlogi-cli fixture::verify
cargo test -p openlogi-fixture --features fs fs::tests
```

| Boundary | Automated contract |
| --- | --- |
| CLI / device operations | Every declared case executes its production read and consumes every required cassette exchange. Settings match the profile. Feature tables, control tables, and battery diagnostics match fixed reviewed expectations. |
| Agent hardware | All recorded DPI, SmartShift, wheel, and backlight reads use the injected backend, exact-route registry, receiver lease, and device-I/O gate. An unpublished route and a suspended gate refuse the operation. Resuming permits the recorded read on one channel. |
| Mock Agent / IPC | Every profile supplies the real mock RPC handlers. Tests check setting support, DPI and SmartShift write/readback, snapshots, and frozen-time observations. Unix tests run the production handshake and bincode socket transport in an isolated process. |
| Desktop | Every profile enters `AppState::apply_agent_snapshot`. Tests check identity, routes, battery, measured capabilities, expected detail tabs, and stable identity across offline/online snapshots. |

The operation expectation file is [`corpus_diagnostics.json`](../crates/openlogi-cli/src/cmd/fixture/record_case/corpus_diagnostics.json). Each feature tuple is `[feature_id, version, type_bits]`; each control tuple is `[control_id, task_id, capability_bits]`. A battery-free device states `raw_battery: null`, which is reviewed as the recorded absence of the battery feature rather than a diagnostic string. The initial tables were transcribed from reviewed response payloads, independently of the production decoder. Tests never regenerate expectations. When adding a specimen, review and add its exact case set, diagnostics, and desktop tab expectations. Unknown operations, missing expectations, deleted reviewed specimens, and changed case coverage fail.

All suites run under the existing workspace test jobs. CI selects those jobs when source, fixtures, test resources, dependencies, toolchain settings, or CI configuration change. Pure Markdown and unrelated design assets skip the test runners. The path filter lives in the `fmt` job of [ci.yml](../.github/workflows/ci.yml); local `cargo test` commands always run the requested suites. Linux and Windows run device and Agent tests; macOS also runs desktop tests. Unix socket isolation is unavailable on the fixed Windows pipe, so Windows runs the in-memory RPC suite instead.

These contributions contain individual read operations, not full discovery sessions. Agent tests publish an opened recorded route as fixture setup; separate synthetic enumeration tests cover discovery and reconnect topology. The corpus replay helper rejects multi-device and raw-HID profiles instead of guessing an operation target. Firmware cases remain excluded pending privacy classification. Mock writes, offline transitions, and replay do not establish physical write behavior, gestures, haptics, radio reconnects, or OS permissions.

## Contribute a device fixture

Use the contribution wizard instead of writing `manifest.json`, synthetic
identities, case relationships, or occurrence counts by hand:

```sh
openlogi fixture contribute \
  --id mx-master-3s-001 \
  --name "MX Master 3S" \
  --device "MX Master 3S" \
  --output fixtures/devices/mx-master-3s-001
```

The first run talks only to the running Agent and writes a privacy-safe semantic
profile plus resumable state. It then asks you to stop the Agent and rerun the
same command. The second run uses the CLI's own HID permission to capture all
eight supported read-only operations, self-replays them, generates the exact
identity ledger and case relationships, and runs strict on-disk verification.
Nothing is uploaded automatically.

Before selecting a target, direct discovery may enable wireless notifications
and request arrival reports on connected receivers. Notification flags are not
restored. The CLI warns before discovery; captured operations do not change
device settings or pairings.

Use `--profile-only` when direct HID access is unavailable. Standalone raw-HID
devices automatically produce profile-only fixtures because the cassette
format covers HID++, not raw device writes. A profile-only contribution is
still useful for the mock Agent and desktop tests.

Keep the same physical device connected between both runs. On macOS the Agent
and CLI are separate Input Monitoring identities, so permission granted to one
does not grant it to the other. The wizard deliberately does not add raw
recording to Agent IPC.

The small built-in synthetic profile is packaging data rather than captured
corpus. It stays under
`crates/openlogi-fixture/fixtures/devices/openlogi-canonical-synthetic-001/` so
the published `openlogi-fixture` crate and mock agent remain self-contained.
It intentionally declares no recorded cases or hardware provenance.

## MX Master 4 capture

`devices/mx-master-4-001` was captured on macOS on 2026-10-10 through a Bolt receiver (`046d:c548`, slot 2). The mouse reports model ID `0xb042`. The semantic profile came from CLI and Agent 0.8.11; the HID++ cassettes came from the repository CLI 0.8.13. Receiver and device identities are synthetic.

The fixture contains seven cases: feature table, reprogrammable controls, raw battery, DPI, SmartShift, wheel mode, and unsupported backlight state. The DPI capture reports 2000 DPI with a supported range of 200–8000 in steps of 50. The control table has nine entries, including separate gesture (`0x00c3`) and haptic panel (`0x01a0`) controls. Regression tests replay DPI and control reads through the production operations and require complete cassette consumption.

The firmware-entities case is excluded because nonzero `extra_version` bytes and entity flags beyond the active bit remain unclassified. The generated manifest retains the seven accepted cases and their existing identity occurrences. This capture does not verify setting writes, haptic output, gestures, or physical wheel behavior. The Agent and Overlay were restored after capture.

## MX Anywhere 3S capture

`devices/mx-anywhere-3s-001` was captured on macOS 26.4 on 2026-10-10 through Bluetooth LE (`046d:b037`). The semantic profile came from CLI and Agent 0.8.11; the HID++ cassettes came from the repository CLI 0.8.13. Device identities are synthetic.

The fixture contains seven cases: feature table, reprogrammable controls, raw battery, DPI, SmartShift, wheel mode, and unsupported backlight state. All 65 exchanges use 20-byte long reports. The DPI capture reports current and default values of 1000 DPI, with a supported range of 200–8000 in steps of 50. The control table has seven entries, including a physical control (`0x00c4`) with raw XY and analytics flags and a virtual control (`0x00d7`) with forced raw XY. Regression tests replay DPI and control reads through the production operations, exercise Bluetooth short-to-long request widening, and require complete cassette consumption.

The firmware-entities case is excluded because nonzero `extra_version` bytes remain unclassified. The generated manifest retains the seven accepted cases and removes the excluded case's unit-ID occurrence. This capture does not verify setting writes, gestures, physical wheel behavior, or reconnects. The Agent and Overlay were restored after capture.

## MX Master 3S capture

`devices/mx-master-3s-001` was captured on macOS 26.7.1 (25G241) on 2026-10-10 through Bluetooth LE (`046d:b034`). The semantic profile came from CLI and Agent 0.8.11; the HID++ cassettes came from the repository CLI 0.8.13. Device identities are synthetic.

The fixture contains seven cases: feature table, reprogrammable controls, raw battery, DPI, SmartShift, wheel mode, and unsupported backlight state. All 68 exchanges use 20-byte long reports. The DPI capture reports current and default values of 1000 DPI, with a supported range of 200–8000 in steps of 50. The control table has eight entries, including a gesture control (`0x00c3`, task `0x00a9`) with raw XY and analytics flags and a virtual control (`0x00d7`) with forced raw XY.

This specimen exposes legacy SmartShift (`0x2110`): ratchet mode, an auto-disengage threshold of 10, and no tunable torque. Regression tests replay SmartShift and control reads through the production operations and require complete cassette consumption. The SmartShift case covers two missing-`0x2111` probes followed by the `0x2110` read.

The firmware-entities case is excluded because two entities contain nonzero `extra_version` bytes that remain unclassified. The generated manifest retains the seven accepted cases and removes the excluded case's unit-ID occurrence. This capture does not verify setting writes, gestures, physical wheel behavior, reconnects, or alternate transports. The GUI, Agent, and Overlay were restored after capture.

## MX Master 3 capture

`devices/mx-master-3-001` was captured on macOS 26.5.1 (25F80) on 2026-10-27 through Bluetooth LE (`046d:b023`), with main firmware MPM19.01_0015. The semantic profile came from CLI and Agent 0.8.18; the HID++ cassettes came from the release CLI 0.8.18. Device identities are synthetic.

The fixture contains seven cases: feature table, reprogrammable controls, raw battery, DPI, SmartShift, wheel mode, and unsupported backlight state. All 64 exchanges use 20-byte long reports. The DPI capture reports current and default values of 1000 DPI, with a supported range of 200–4000 in steps of 50. The control table has the same eight entries as the MX Master 3S, including a gesture control (`0x00c3`, task `0x00a9`) and a virtual control (`0x00d7`) with forced raw XY. The feature table has 31 entries, including Thumbwheel (`0x2150`).

This specimen exposes legacy SmartShift (`0x2110`) in free-spin mode, with an auto-disengage threshold of 10 and no tunable torque. Its battery uses the legacy `0x1000` feature instead of `0x1004`. The mouse was charging: the raw read reports `discharge_level=0` and `Recharging`, while the Agent-derived profile reports 20 % (low, charging), because the Agent holds the last discharge reading while a legacy `0x1000` device charges. Regression tests replay DPI and SmartShift reads through the production operations and require complete cassette consumption. The SmartShift case covers two missing-`0x2111` probes followed by the `0x2110` read.

The firmware-entities case is excluded because the bootloader and application entities contain nonzero `extra_version` bytes that remain unclassified. The generated manifest retains the seven accepted cases and removes the excluded case's unit-ID occurrence. This capture does not verify setting writes, gestures, thumbwheel or physical wheel behavior, reconnects, or the receiver transport the device also advertises. The Agent was restored after capture.

## MX Mechanical Mini for Mac capture

`devices/mx-mechanical-mini-mac-001` was captured on macOS 26.5.1 (25F80) on 2026-10-27 through Bluetooth LE (`046d:b36d`), with main firmware RBK75.01_0014. It is the first keyboard in the corpus. The semantic profile came from CLI and Agent 0.8.18; the HID++ cassettes came from the release CLI 0.8.18. Device identities are synthetic, and Easy-Switch host names are replaced with `Computer 1`–`Computer 3`.

The fixture contains seven cases: feature table, reprogrammable controls, raw battery, backlight state, and unsupported DPI, SmartShift, and wheel mode reads. All 87 exchanges use 20-byte long reports. The feature table has 33 entries, including Backlight2 (`0x1982` version 2), Fn inversion (`0x40a3`), DisableKeys (`0x4521`), MultiPlatform (`0x4531`), and ChangeHost/HostsInfo (`0x1814`/`0x1815`). The battery read uses `0x1004` version 3 and reports 100 %, full, discharging.

The control table has 32 entries, and none carries the mouse flag. Twelve Fn-row positions carry the function-key and Fn-toggle flags. The first three are the Easy-Switch host keys (`0x00d1`–`0x00d3`), which are neither reprogrammable nor divertable. Volume down and up (`0x00e8`/`0x00e9`) are reprogrammable, divertable hotkeys, and 16 further keys are reprogrammable and divertable without Fn flags. The last two controls (`0x00de`, `0x0034`) carry only the function-key or hotkey flag. Every control reports analytics key events.

The backlight is enabled with eight levels, current level 0, and a temporary-manual status. Its options support only power save and the wow effect, not automatic or manual modes, so power save is the only control offered beyond on/off. The profile also records Fn lock on (default off), three paired Bluetooth hosts, and Caps Lock as the only key that can be disabled. Regression tests replay control and backlight reads through the production operations and require complete cassette consumption. The desktop shows Keys, Actions Ring, and Device tabs.

The firmware-entities case is excluded because the bootloader and application entities contain nonzero `extra_version` bytes that remain unclassified. The generated manifest retains the seven accepted cases and removes the excluded case's unit-ID occurrence. This capture does not verify setting writes, key diversion, host switching, Fn-lock or OS-layout behavior, backlight level changes, or reconnects. The Agent was restored after capture.

## MX Creative Keypad capture

`devices/mx-creative-keypad-001` was captured on macOS 26.0 on 2026-10-12 as a direct wired USB device (`046d:c354`, model ID `0xc354`, extended model `0x02`, transports USB only). The semantic profile came from CLI and Agent 0.8.13; the HID++ cassettes came from the same repository CLI. Device identities are synthetic.

The fixture contains seven cases: feature table, reprogrammable controls, raw battery, and unsupported DPI, SmartShift, wheel, and backlight state. The feature table has 18 entries, including Reprogrammable Controls (`0x1b04`, version 6) and Brightness (`0x8040`, version 1). The device exposes no battery feature, so this is the first corpus specimen with a reviewed `raw_battery: null` expectation. The raw-battery case answers each battery-feature lookup (`0x1004`, `0x1000`, `0x1001`, `0x1F20`) with the absence the feature table records; the `0x1001` exchange was added after capture, when the diagnostic read gained that lookup. The control table has exactly two entries, `0x01a1` and `0x01a2` (the left and right paging keys), both task `0x0000` with divertable-only flags (`0x20`). The nine LCD grid keys are not HID++ controls: they arrive as `0x13` vendor reports, are not represented in the control table or the fixture, and remain out of scope. The device resolves its `numpad` kind from the in-repo keypad identity registry, not the external asset depot. Regression tests replay the feature and control tables through the production operations and require complete cassette consumption.

The firmware-entities case was recorded by the wizard but excluded because firmware observations need independently reviewed expectations, consistent with the other captures. This capture does not verify setting writes, key notifications, vendor-report handling, or reconnects. No OpenLogi process was running before the capture; the capture agent was stopped afterwards.

## MX Keys S capture

`devices/mx-keys-s-001` was captured on macOS 26.7.1 (25G241) on 2026-10-30 through Bluetooth LE (`046d:b378`), with main firmware RBK81.03_B0018. The semantic profile came from the development Agent and the HID++ cassettes from the repository CLI, both 0.8.19 at `cbb754e`. Device identities are synthetic, and Easy-Switch host names are replaced with `Computer 1` and `Computer 2`.

The fixture contains seven cases: feature table, reprogrammable controls, raw battery, backlight state, and unsupported DPI, SmartShift, and wheel mode reads. All 77 exchanges use 20-byte long reports. The feature table has 34 entries, including Backlight2 (`0x1982` version 3), Fn inversion (`0x40a3`), DisableKeys (`0x4521`), MultiPlatform (`0x4531`), and ChangeHost/HostsInfo (`0x1814`/`0x1815`). The battery read uses `0x1004` version 3 and reports 90 %, full, discharging.

The control table has 21 entries, and none carries the mouse flag. Twelve Fn-row positions carry the function-key and Fn-toggle flags; with five hotkeys, seventeen keys are reprogrammable and divertable. The Easy-Switch host keys (`0x00d1`–`0x00d3`) are hotkeys that are neither reprogrammable nor divertable, and `0x00de` carries no capability flag beyond analytics key events. The expected feature and control tables were taken from live `openlogi diag features` and `openlogi diag controls` reads of the same keyboard, not from the replay output.

The backlight is enabled with eight levels, current level 0, and a permanent-manual status, with automatic, level, power-save, and duration controls and a 15-second duration. The profile also records Fn lock on (default off), two paired hosts (one Bolt, one Bluetooth) out of three channels, and five keys that can be disabled. The desktop shows Keys, Actions Ring, and Device tabs.

The firmware-entities case is excluded because the softdevice and hardware entities contain unclassified name and version bytes. The manifest retains the seven accepted cases and removes the excluded case's unit-ID occurrence. This capture does not verify setting writes, key diversion, host switching, Fn-lock or OS-layout behavior, backlight level changes, or reconnects. The Agent and Overlay were restored after capture.
