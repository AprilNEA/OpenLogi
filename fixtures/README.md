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
