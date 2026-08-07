# Litra and standalone-light integration plan

This document is the working plan for OpenLogi issue [#144](https://github.com/AprilNEA/OpenLogi/issues/144): support non-HID++ Logitech device categories, starting with Litra lights.

The camera-link behavior follows Logitech's documented **Activate Light with
Camera** feature: it links LITRA GLOW/BEAM power to any webcam, virtual camera,
capture card, or SLR camera. OpenLogi implements the same policy locally using
the host camera-use signal rather than an application-specific integration.
Reference: [Logitech Support](https://support.logi.com/hc/en-nz/articles/10762678342551-What-is-the-Activate-Light-with-Camera-feature-in-G-HUB).

The target hardware currently connected to the development Mac is a Logitech Litra Glow. The local HID inventory confirms the expected device shape:

```text
manufacturer:       Logi
product:            Litra Glow
vendor_id:          0x046d
product_id:         0xc900
usage_page:         0xff43
usage:              0x0202
max_output_report:  20 bytes
```

The device-specific serial number is intentionally not recorded in this document.

## Objective and MVP boundary

The MVP should provide local, manual control of a Litra light while establishing the abstraction for the rest of the Litra family and for other standalone lights:

- discover the device over USB;
- expose it as a first-class OpenLogi device;
- turn it on and off;
- set brightness;
- set colour temperature;
- persist the last requested values per physical device;
- re-apply persisted values after reconnect;
- expose a small CLI surface before the GUI surface;
- expose a dedicated Light panel in the GUI;
- optionally link a light's power to host camera activity through a persisted,
  capability-neutral toggle.

Profiles, presets, firmware updates, and every Litra model remain outside the
MVP. Camera automation is deliberately implemented as a generic standalone-light
policy: another Litra model or another camera-capable light can opt into the
same setting without a new inventory model, persistence format, or GUI branch.

## Architectural verdict

The first version of the plan was too Glow-oriented in two places: it proposed lumen-valued settings as the persistence primitive, and it left the raw device inside the existing HID++-centric inventory path. That would solve one device but make the next light progressively more expensive to add.

The corrected direction is:

1. `DeviceKind::Light` remains a coarse identity classification only. It must not be used to decide which controls a light supports.
2. The existing `Capabilities` type remains HID++-specific. In this codebase, `Capabilities::lighting` means the keyboard RGB path driven by HID++ `0x8070`/`0x8080`; it must not be reused for Litra.
3. Add a protocol-neutral light capability descriptor with optional, typed controls: power, brightness range/unit/step, temperature range/step, and future colour or zone controls. A device advertises only the controls its driver can actually implement.
4. Keep raw standalone devices separate from `PairedDevice`. `DeviceInventory` currently models receivers and their HID++ pairing slots; putting a Litra into that struct would make the wrong abstraction look reusable. Add a standalone-device collection to the agent snapshot (or the equivalent canonical inventory DTO), then normalize both sources into the existing agent/GUI device-record pipeline.
5. Make raw HID addressing generic and protocol drivers pluggable inside `openlogi-hid`: Litra is the first driver, not the device model. A future Litra variant should be a product-description/matcher entry; a non-Litra light should be another driver implementing the same semantic light operations.
6. Persist device-neutral settings where possible. Store brightness as a normalized percentage and temperature as Kelvin; map those values to lumens, steps, or another protocol unit inside the driver. The CLI may accept a device-native convenience flag such as `--lumens`, but raw device units must not leak into the shared config contract.

This is deliberately an additive migration. Existing HID++ receivers, routes, RGB keyboard lighting, and their wire representations keep their current meaning while standalone devices are introduced beside them. The compatibility adapter can later be retired once all inventory consumers use the canonical device descriptor.

## Development requirements

### Required and verified on the current Mac

| Requirement | Expected | Current result |
|---|---:|---|
| macOS | 13+ | macOS 26.5.2 |
| Architecture | Apple Silicon or supported Intel target | `arm64` |
| Xcode | 16+ full installation | Xcode 26.6 |
| Metal toolchain | `xcrun --find metal` must resolve | Available |
| Rust | stable, MSRV 1.96 | rustup stable 1.97.1 |
| Cargo | from the rustup toolchain | Cargo 1.97.1 |
| Clang | available for native dependencies | Apple Clang 21.0.0 |
| CMake | useful for native dependency builds | CMake 3.28.0 |
| Homebrew | optional but useful on macOS | Homebrew 6.0.14 |
| Litra reference CLI | optional hardware oracle | `litra` 3.3.0 installed |

The repository declares a stable toolchain in `rust-toolchain.toml` and an MSRV of Rust 1.96. Homebrew Rust 1.85 was not sufficient for this workspace, so development commands must resolve rustup before `/opt/homebrew/bin/rustc` and `/opt/homebrew/bin/cargo`.

For the current shell:

```sh
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
rustc --version
cargo --version
rustup show active-toolchain
```

No shell startup file was modified automatically. To make this permanent, add the PATH export to the user's preferred shell configuration manually.

### Optional tools

- `devenv` / Nix: repository-supported alternative environment; not required.
- `create-dmg`: required only for local DMG packaging, not for Rust development.
- `sccache`: optional build acceleration.
- `litra`: reference implementation and hardware sanity check; it is not an OpenLogi runtime dependency.

### Hardware and runtime conditions

- Connect the Litra over USB.
- Close Logi Options+, G Hub, and other Logitech control applications before raw-HID tests; they can compete for the device handle.
- Raw light control does not require the mouse Accessibility permission. The GUI may still ask for Accessibility because the existing agent owns the mouse event hook; that permission is unrelated to the Litra driver.
- The device must be testable without changing or committing its serial number or other personal hardware identifiers.

## Protocol facts to preserve

The issue describes fixed 20-byte HID output reports on a vendor-defined HID collection. The command payloads are:

```text
power on:     [0x11, 0xff, 0x04, 0x1c, 0x01]
power off:    [0x11, 0xff, 0x04, 0x1c, 0x00]
brightness:   [0x11, 0xff, 0x04, 0x4c, <u16 lumens>]
temperature:  [0x11, 0xff, 0x04, 0x9c, <u16 kelvin>]
```

Every report must be padded to exactly 20 bytes. The byte order for the two `u16` values must be taken from the reference implementation and locked down with tests; it must not be guessed from the UI range.

For the Litra Glow, the user-facing ranges are 20–250 lumen and 2700–6500 K. The driver should validate typed values before encoding them. Temperature steps should follow the device/reference implementation, expected to be 100 K increments.

Reference implementations:

- [`timrogers/litra-rs`](https://github.com/timrogers/litra-rs) for the current Rust device model and supported ranges.
- [`timrogers/litra`](https://github.com/timrogers/litra) for the earlier JavaScript implementation.
- [`async-hid`](https://docs.rs/async-hid/latest/async_hid/) for the Rust raw-HID API used by OpenLogi; its macOS backend uses `IOHIDManager`.

## Recommended architecture

The central rule is to keep raw HID and HID++ separate. A Litra is not a HID++ feature device and must not be made to look like one just to reuse mouse code.

### `openlogi-core`

- Extend `DeviceKind` with `Light` as an append-only serialized variant.
- Keep `DeviceKind` identity-only, following the existing rule that measured
  capabilities — not kind — gate panels.
- Keep `Capabilities` as the HID++ feature-derived DTO. Do not add a generic
  `lighting` meaning to its existing boolean: that field currently means the
  keyboard RGB implementation and changing its semantics would regress existing
  devices.
- Introduce a separate, serializable light descriptor, for example
  `LightCapabilities`, with optional typed controls:

  ```rust
  struct LightCapabilities {
      power: bool,
      brightness: Option<ScalarRange>,
      temperature: Option<TemperatureRange>,
      color: Option<ColorCapabilities>,
      zones: Option<ZoneCapabilities>,
  }
  ```

  `ScalarRange` must carry the supported range and step; its unit may be
  lumens, percentage, or another explicitly named unit. `color` and `zones`
  are optional groundwork for future multi-zone lights, not requirements for
  the Glow MVP.
- Add a standalone-device DTO rather than extending `PairedDevice` with raw-
  device fields. It should carry the same stable identity/display/online/
  capabilities information the GUI already consumes, plus a generic raw
  device address. Existing receiver inventories remain intact during the
  migration.
- Introduce light-specific settings, rather than reusing the keyboard RGB
  `Lighting` struct:

  ```rust
  struct LightSettings {
      enabled: bool,
      auto_camera: bool,
      brightness_percent: u8,
      temperature_kelvin: Option<u16>,
      color: Option<Rgb>,
  }
  ```

- `color` stays optional and unused by the Glow MVP. A device with no colour
  capability must not expose a colour control through a generic UI fallback.
- Map `brightness_percent` to the device's native range in the backend. This
  keeps configuration portable across a 250-lumen Glow, another Litra model,
  and a light whose protocol uses percentages or discrete steps. CLI parsing
  can still offer `--lumens` when the advertised range has lumen units.
- Add a stable physical identity strategy for raw devices. Prefer a device
  serial when available; when the HID backend exposes only an OS-node identity,
  classify it as transient and do not persist it as a physical configuration
  key. Never use a transient inventory index or “first matching device” as a
  configuration key.
- Add serde defaults and migration tests for existing TOML files.

Relevant existing areas: `crates/openlogi-core/src/device.rs`,
`crates/openlogi-core/src/color.rs`, `crates/openlogi-core/src/config.rs`, and
`crates/openlogi-core/src/config/device.rs`.

### `openlogi-hid`

- Split device discovery into a general raw-HID enumeration layer and a HID++ candidate layer.
- Keep the existing HID++ filter unchanged for mice/keyboards/receivers.
- Add a generic raw-device descriptor and a dedicated `RawHid` route containing
  the fields needed to re-find an interface. Do not overload `Direct`, whose
  semantics currently mean a directly attached HID++ device addressed at
  index `0xff`.
- Extend `DeviceStableId`/`PhysicalDeviceKey` for the raw route rather than
  introducing a parallel identity format. The key must include the raw-device
  identity and enough of the matching tuple to prevent two identical lights
  from sharing settings.
- Match a device using the full identity tuple (vendor ID, product ID, usage
  page, usage) plus the stable identity when duplicates are possible. If two
  indistinguishable nodes remain, return an explicit ambiguity error instead
  of silently selecting the first one.
- Add a small driver-dispatch layer, with one module per protocol/family. The
  first entry is `litra.rs`; it maps known Litra product IDs to a variant
  descriptor and implements semantic light operations. A future Litra model
  should normally add a table entry and capability mapping, not a new route or
  GUI protocol. A different light protocol gets another driver module, not
  another special case in the agent.
- Keep protocol encoding in the driver: typed commands, range validation,
  report encoding, exact report padding, and the raw writer call. The shared
  layer should not know that Litra uses report IDs `0x11`/`0x12` or 20-byte
  reports.
- Serialize writes per physical device and apply the same bounded timeout policy used by existing hardware writes.
- Preserve the inventory cache/ledger behaviour so a temporarily unavailable device is not treated as a permanent removal.

Relevant existing areas: `crates/openlogi-hid/src/transport.rs`, `route.rs`,
`inventory.rs`, `write.rs`, `crates/openlogi-agent-core/src/device_order.rs`,
and the existing raw `DeviceWriter` path in `write/lighting.rs`.

### `openlogi-agent-core` and `openlogi-agent`

- Keep all device I/O in the agent; the GUI must remain an IPC client.
- Extend the inventory/orchestration model so a standalone raw light can
  coexist with HID++ receivers and direct HID++ devices. Keep the current
  `inventory()` contract as the HID++ compatibility view while adding the
  standalone collection to the atomic snapshot/canonical device view. Do not
  synthesize a fake receiver merely to fit a raw light into `PairedDevice`.
- Normalize receiver-backed and standalone descriptors into the same stable
  device/config-key pipeline. The existing cache, offline identity replay,
  ordering, and duplicate suppression rules must apply to both sources.
- Append light operations to the tarpc `Agent` service; do not reorder existing methods.
- Append new snapshot fields and service methods only; bump `PROTOCOL_VERSION`
  and update the bincode wire-format goldens for every serialized type or
  service change. In particular, adding `Light` to `DeviceKind`, adding raw
  route variants, and exposing standalone devices are all wire changes.
- Add `LightSettings` reload/re-apply handling for reconnects.
- Keep writes idempotent and sequential per device to avoid two GUI slider
  releases racing on the same HID handle. A generic light command should be
  expressed in semantic values (enabled, percentage, Kelvin, optional colour),
  with native-unit conversion delegated to the selected driver.

Relevant existing areas: `crates/openlogi-agent-core/src/ipc.rs`, `orchestrator.rs`, `hardware.rs`, and `crates/openlogi-agent/src/server.rs`.

### CLI

Implement the smallest useful hardware-facing surface first, for example:

```text
openlogi light list
openlogi light on
openlogi light off
openlogi light brightness --lumens 150
openlogi light temperature --kelvin 4500
```

The CLI makes protocol and route testing possible without GPUI, and its error messages will help diagnose discovery and device-ownership problems.

Relevant existing areas: `crates/openlogi-cli/src/lib.rs`, `cmd/list.rs`, and `cmd/diag.rs`.

### GUI

- Add `Light` to the device-kind labels and inventory card rendering.
- Add a dedicated Light tab/panel, not the existing keyboard RGB panel. The
  existing `Lighting` tab remains the HID++ keyboard-RGB panel.
- Gate the new tab on `light_capabilities`, not on `DeviceKind::Light`, so a
  misclassified device cannot receive an inert panel and a future light can
  advertise only the controls its driver supports.
- Render controls from the descriptor: power when supported, normalized
  brightness mapped to the advertised native range, temperature when
  supported, and colour/zones only when their capability is present.
- Keep the UI optimistic only for the duration of an accepted command; surface failures from the agent.
- Persist settings through `AppState` and the existing TOML reload path.
- Add English strings first, then update the locale workflow as required by the repository.

Relevant existing areas: `crates/openlogi-gui/src/app.rs`, `app/detail.rs`, `state.rs`, `state/devices.rs`, and `components/lighting_panel.rs`.

## Existing patterns this work must replicate

These are not optional style preferences; they are the behaviours that keep
the current device support reliable:

- **Capabilities over kind.** `DetailTab::tabs_for` gates panels from measured
  `Capabilities`, with a narrowly-scoped offline fallback from `DeviceKind`.
  The Light tab must use the new light descriptor and must not infer controls
  from `DeviceKind::Light` alone.
- **Identity over inventory position.** `DeviceStableId`, `PhysicalDeviceKey`,
  persisted `DeviceIdentity`, and the GUI's offline placeholders keep a device
  and its configuration stable across sleep, restart, and enumeration order.
  The raw path must join that same identity pipeline.
- **Cache/ledger grace.** `openlogi-hid/src/inventory` continues to own the
  HID++ node ledger, while the agent watcher owns a per-interface raw-node
  ledger. An OS-level enumeration failure keeps the last agent snapshot; a
  successful raw omission is retained offline for a bounded grace and is then
  treated as a real detach. The standalone DTO and route remain unchanged.
- **Single I/O owner.** The agent owns discovery, handles, serialization,
  timeouts, and re-application for the GUI/runtime path. The existing CLI is
  also a diagnostic hardware surface and follows the repository's established
  direct `openlogi-hid` pattern; its Litra commands reuse the exact same typed
  driver and never duplicate transport or encoding logic.
- **Typed protocol boundaries.** Follow the existing typed wrappers and error
  types in `openlogi-hid`: validate values before encoding, expose unsupported
  wire values as errors, and keep reverse-engineered offsets documented and
  tested.
- **Append-only wire evolution.** `DeviceKind`, `DeviceRoute`, snapshot fields,
  and tarpc methods cross bincode/tarpc. Add variants/fields/methods only at the
  end, bump `PROTOCOL_VERSION`, and update the golden wire tests in the same
  change.
- **No premature plugin system.** Extensibility here means a small internal
  driver interface and a static dispatcher with clear ownership, not dynamic
  third-party plugins. A registry can be considered later if the number of
  protocols justifies it; the first PR should keep the execution and security
  model easy to audit.

The main code-review risk is accidentally creating a second device pipeline
for lights. The implementation should have one canonical device/config path,
with HID++ receiver entries and raw standalone entries adapted at the source.

## Test strategy

The feature should be developed test-first at the pure boundaries and tested
against real hardware only at the transport boundary. A connected Litra is
useful for confirmation, but it must not be required for the normal edit/test
loop.

### Existing suites to preserve and extend

- `openlogi-core`: device-kind, capability, TOML, RGB, identity, and diagnostic
  tests. Adding `Light` must update exhaustive mappings and wire fixtures
  without changing existing HID++ expectations.
- `openlogi-hid`: route, raw writer, inventory cache/ledger, probe, and error
  tests. Existing HID++ enumeration tests must prove that the new raw layer
  does not widen the HID++ candidate filter or alter receiver behaviour.
- `openlogi-agent-core`: orchestrator, stable ordering, re-apply, config-key,
  IPC, and bincode golden tests. Existing mouse/keyboard snapshots remain
  valid and the new standalone collection gets its own fixtures.
- `openlogi-gui`: capability-gated tab tests, device-list/identity tests,
  route/connection rendering, and localization checks. Existing keyboard RGB
  lighting tests must remain separate from the new Light panel tests.

### New automated tests

1. **Core model and configuration**
   - `DeviceKind::Light` serializes/deserializes at the appended enum position.
   - Light capabilities omit unsupported controls and preserve range/step/unit.
   - Old TOML without light fields still deserializes byte-for-byte
     semantically; new fields use explicit serde defaults.
   - Normalized brightness and optional Kelvin values round-trip correctly.
   - Two physical raw devices cannot accidentally share a persistent key.

2. **Pure Litra protocol tests**
   - Exact power-on and power-off reports.
   - Exact brightness and temperature reports, including confirmed byte order.
   - Exactly 20-byte report padding and report IDs.
   - Lower/upper boundary values, step quantization, and every invalid value.
   - Percentage-to-native-range conversion, including rounding and clamping
     policy; unsupported colour/zone operations return typed errors.
   - No test writes to a real HID node.

3. **Raw discovery and routing**
   - Positive and negative matcher fixtures for vendor/product/usage tuples.
   - Litra variants select the expected driver/capability profile.
   - HID++ candidates remain limited to the existing collection filters.
   - Stable identity prefers serial, rejects transient identity, and handles
     duplicate indistinguishable nodes with an explicit ambiguity error.
   - `RawHid` never enters HID++ `Direct` channel-opening code.

4. **Inventory and reconnect behaviour**
   - A raw light appears in the standalone collection, not as a paired slot.
   - Receiver-backed and standalone devices coexist in deterministic order.
   - One failed poll replays the last good raw descriptor; repeated absence
     eventually removes it according to the same grace policy as HID++.
   - Unplug/replug preserves the config key and re-enriches online state.

5. **Agent, IPC, and persistence**
   - Snapshot and service changes are append-only and the protocol version is
     pinned by the wire-format golden tests.
   - Commands are serialized per physical device and failures do not partially
     commit configuration.
   - Saved light settings re-apply on first discovery, reconnect, and agent
     restart; existing DPI/RGB/SmartShift re-apply tests remain unchanged.
   - Camera-linked power changes only the effective `enabled` value, preserves
     the manual setting, and is covered for both camera states and config
     round-trips.

6. **GUI and CLI**
   - A light with only power/brightness gets only those controls; temperature,
     colour, and zones appear only when advertised.
   - Keyboard RGB still uses the existing `Lighting` panel and path.
   - CLI parsing and validation are tested without a running agent; IPC error
     rendering is tested with typed failure fixtures.

### Hardware test boundary

The real Litra is required only for a small manual matrix: discovery, on/off,
brightness, temperature, unplug/replug, and agent restart. The manual test
must record model and observed result, never serial numbers. The PR must list
automated commands separately from hardware checks and explicitly state what
was not tested on physical hardware.

## Implementation milestones

### Milestone 0 — baseline and hardware reconnaissance

Status: complete.

- Repository clean on `master`.
- Mac/Xcode/Metal checked.
- Rustup stable installed and selected.
- Litra identified by `ioreg`.
- Reference `litra devices --json` sees a Litra Glow and reports the expected ranges.
- No device state was changed during reconnaissance.

### Milestone 1 — shared light contracts

Status: complete.

1. Define the standalone-device DTO and the light capability descriptor in
   `openlogi-core`; do not add raw-light fields to `PairedDevice`.
2. Define semantic light settings/commands and their serde defaults. Keep
   brightness normalized in persisted config and keep native units in drivers.
3. Add `DeviceKind::Light` at the end of the enum and update every exhaustive
   mapping (diagnostics, labels, cards, tests) without changing existing
   variant order.
4. Add pure tests for range/step validation, config migration, capability
   omission, and stable-identity rules.

Exit criterion: the core model can describe a Glow, a hypothetical Litra with
different ranges, and a non-Litra light without importing `hidpp` or knowing a
product ID.

### Milestone 2 — isolated Litra protocol driver

Status: complete for Litra Glow; additional models remain data/driver follow-ups.

1. Confirm the byte order and exact command encoding from `litra-rs`.
2. Add typed Litra variant descriptors and product matching. Keep the product
   table in the driver module, not in `openlogi-core`.
3. Add pure tests for all four commands, padding, ranges, invalid values,
   normalized-brightness mapping, and unsupported optional controls.
4. Add a raw-HID discovery test using synthetic `DeviceInfo`-like data or a
   matcher helper.
5. Add a diagnostic-only path before changing the full inventory.

Exit criterion: the driver can produce exact 20-byte reports and can issue a controlled command to the connected Litra without involving the GUI.

### Milestone 3 — raw-device inventory and stable routing

Status: complete for discovery, atomic inventory integration, bounded per-node
raw omission grace, and the connected Litra Glow unplug/replug check.

1. Generalize enumeration while preserving the existing HID++ filter.
2. Add `RawHid` routing and stable identity handling.
3. Add the standalone collection to the atomic agent snapshot and normalize it
   into the current device ordering/config pipeline.
4. Preserve cache/ledger replay and offline identity behaviour for raw devices.
5. Add reconnect/disconnect tests, duplicate-device tests, and a test proving
   a raw light is not mistaken for a HID++ direct device.

Exit criterion: `openlogi light list` discovers the Litra and the agent can re-find it after a USB reconnect.

### Milestone 4 — agent IPC and persistence

Status: complete.

1. Add light settings to the device configuration with backward-compatible serde defaults.
2. Append IPC commands and bump the protocol version.
3. Implement agent-side bounded, serialized writes.
4. Re-apply saved state after the device returns.
5. Update wire-format tests.

Exit criterion: CLI/GUI commands go through the agent, values survive restart, and an unplug/replug cycle restores the saved state.

### Milestone 5 — GUI

Status: complete for the capability-driven Light panel and locale parity.

1. Add the light device kind and card presentation.
2. Add the dedicated Light panel.
3. Add localization strings.
4. Test with both a Litra and an existing HID++ mouse/keyboard connected so the existing panels remain unchanged.

Exit criterion: the Litra appears as a light and exposes only relevant controls.

### Milestone 6 — camera-linked light automation

Status: implemented on macOS; the policy is generic and the OS probe is
isolated so equivalent Linux/Windows providers can be added later.

1. Poll CoreMediaIO's aggregate `DeviceIsRunningSomewhere` property so physical
   webcams, virtual cameras, capture cards, and SLR devices are treated alike.
2. Add a per-light persisted `auto_camera` toggle; keep brightness, colour
   temperature, and the manual preference independent from transient power.
3. Apply the effective power state in the agent, including reconnects and config
   reloads, and expose the runtime camera state to the GUI render.
4. Keep the toggle and policy on protocol-neutral `LightSettings`, not in the
   Litra Glow driver.

Exit criterion: enabling the toggle turns each opted-in online light on for a
camera-use transition and off when the aggregate camera state becomes idle;
disabling it restores the persisted manual power behavior.

### Milestone 7 — contribution quality and extension check

Status: complete for the MVP implementation, automated verification, and the
maintainer-reported state-changing hardware matrix. The second-model fixture
is retained as a future extension check rather than an MVP gate.

1. Run formatting, clippy, workspace tests, and the macOS GUI build.
2. Document manual hardware verification without recording serial numbers.
3. Add a concise English PR body with `## Summary`, `## Changes`, `## Testing`, and `Fixes #144`.
4. Include hardware-tested and not-hardware-tested claims separately.
5. Add a fixture for a second Litra-like capability profile and verify that it
   changes only the driver descriptor/encoding expectations, not the core
   inventory, IPC, persistence, or GUI architecture.

Exit criterion: adding a second Litra model is a data/driver change unless it
introduces genuinely new controls; adding a different light protocol is one
new driver plus its tests, without modifying HID++ code paths.

## Verification commands

Use rustup first in every shell:

```sh
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"

cargo fmt --all -- --check
cargo test -p openlogi-core
cargo test -p openlogi-hid
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

For the macOS application:

```sh
cargo run -p openlogi-gui
```

The repository's runner creates the development app bundle. Quit an older OpenLogi instance before judging a new GUI build, because the agent/app singleton prevents a second instance from taking over.

Hardware checks should be explicit and conservative:

```sh
litra devices --json
openlogi light list
openlogi light on
openlogi light brightness --lumens 150
openlogi light temperature --kelvin 4500
openlogi light off
```

These commands should only be run when an intentional physical state change is acceptable.

## Current preparation and implementation result

Completed on the development Mac:

- Rustup stable 1.97.1 is installed and selected; the repository's MSRV remains
  1.96. Homebrew Rust 1.85 is not suitable for this workspace.
- Xcode 26.6 and the Metal toolchain are available, as are Apple Clang and
  CMake. `devenv` remains optional and was not installed.
- The optional `litra` 3.3.0 reference CLI is installed.
- The connected device was discovered as a Litra Glow with VID/PID `046d:c900`,
  usage `ff43:0202`, and 20-byte output reports. `openlogi light list` reads
  the device and reports power, 20–250 lumen brightness, and 2700–6500 K in
  100 K steps. The connected device was exercised through the CLI for power,
  brightness, and temperature; invalid percentage, lumen, and Kelvin boundary
  values were also rejected as expected.
- `openlogi-core` now owns the protocol-neutral light capabilities, standalone
  raw-device DTO, normalized persisted settings, and `DeviceKind::Light`.
- `openlogi-hid` now has a generic `RawHid` route, standalone discovery, an
  isolated Litra driver, typed commands/errors, exact fixed-width report
  encoding, range validation, per-device write serialization, and a 2-second
  raw-write timeout.
- The agent inventory/orchestrator and append-only tarpc snapshot/service now
  carry standalone devices; the GUI remains an IPC client and gates its Light
  tab on advertised capabilities. Existing keyboard RGB remains on its
  separate HID++ `Lighting` path.
- Camera-linked power is implemented as a generic `LightSettings.auto_camera`
  toggle. The macOS agent polls CoreMediaIO's running-device property, applies
  transient power to every opted-in online light, and exposes the aggregate
  runtime state to the GUI without changing persisted manual settings.
- The CLI has `openlogi light list|on|off|brightness|temperature`; its tests
  validate selection and argument behaviour without requiring hardware.
- Added/extended tests cover report bytes, padding, endianness, invalid ranges,
  matcher tuples, raw identity keys, TOML round-trips/clamping, inventory
  snapshots, reconnect planning, IPC golden bytes, CLI selection, GUI
  capability gating, and locale parity.
- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets
  -- -D warnings`, and `cargo test --workspace` are green. The workspace test
  run includes the pre-existing ignored doctest; a future-incompatibility note
  remains for transitive `block` and `proc-macro-error2` packages only.

Hardware verification completed on the connected Litra Glow, without recording
the device serial number:

- discovery alongside the MX Master 3S and immediate device visibility;
- power on/off, brightness, and colour-temperature changes;
- unplug/replug with device rediscovery and persisted-state restoration;
- agent restart with the light and mouse remaining available;
- removal of the stale duplicate Litra identity from the local configuration.

The automated implementation and hardware matrix are therefore complete for
the Glow MVP. Packaging checks are not part of development readiness:
`create-dmg` remains optional and is only needed for DMG packaging.

## Implementation record — completed work

This section records what was actually implemented after the original plan,
including the concurrency and rendering fixes added during hardware validation.
It is deliberately more concrete than the design milestones above so a later
review can compare the intended architecture with the code that landed locally.

### 1. Core model, persistence, and identity

- Added `DeviceKind::Light` as an append-only identity classification. Panels and
  controls are still gated by advertised capabilities rather than by this kind.
- Added protocol-neutral `LightCapabilities`, including power support,
  brightness range/unit/step, temperature range/step, and extension points for
  colour/zones.
- Added `LightSettings` with persisted `enabled`, `auto_camera`, normalized
  brightness percentage, optional Kelvin temperature, and optional colour.
- Kept keyboard RGB `Lighting` separate from standalone-light settings.
- Added standalone-device DTOs and serial-backed raw-device identity handling.
  OS-node fallbacks are explicitly transient and are not persisted. A raw
  Litra is not represented as a fake receiver or as a paired HID++ slot.
- Added TOML defaults, round-trip coverage, clamping/validation tests, and
  physical-key tests so transient raw inventory indexes are not persisted.

Primary files:

- `crates/openlogi-core/src/device.rs`
- `crates/openlogi-core/src/config.rs`
- `crates/openlogi-core/src/config/device.rs`
- `crates/openlogi-core/src/config/settings.rs`

### 2. Raw HID and Litra driver

- Added `DeviceRoute::RawHid` with the complete matching tuple and stable
  identity needed to reopen a standalone interface.
- Added standalone raw-HID enumeration without widening the existing HID++
  candidate path.
- Added an isolated Litra driver and product matcher. The driver owns product
  IDs, report IDs, native units, range validation, byte order, and fixed-width
  report encoding.
- Implemented semantic commands for power, brightness, and temperature.
- Litra Glow reports are padded to exactly 20 bytes. Native brightness is
  expressed in lumens at the driver boundary while persistence remains
  percentage-based.
- Added typed unsupported-control and invalid-value errors.
- Added per-device write serialization and bounded HID operations.

Primary files:

- `crates/openlogi-hid/src/route.rs`
- `crates/openlogi-hid/src/transport.rs`
- `crates/openlogi-hid/src/standalone.rs`
- `crates/openlogi-hid/src/write/litra.rs`
- `crates/openlogi-hid/src/write/error.rs`

### 3. Inventory, agent, and IPC

- Added standalone devices to the atomic agent snapshot alongside the existing
  receiver inventory.
- Normalized receiver-backed and raw standalone devices through the same stable
  ordering, config-key, offline-record, and GUI device-record pipeline.
- Appended light commands and the standalone snapshot field to the tarpc/bincode
  contract and updated the protocol version and wire-format goldens.
- Kept all device I/O in the agent. The GUI sends semantic commands through IPC;
  it does not open HID handles.
- Re-apply persisted light settings on first discovery, reconnect, config reload,
  and agent restart paths.

Primary files:

- `crates/openlogi-agent-core/src/ipc.rs`
- `crates/openlogi-agent-core/src/orchestrator.rs`
- `crates/openlogi-agent-core/src/device_order.rs`
- `crates/openlogi-agent/src/main.rs`
- `crates/openlogi-agent/src/server.rs`

### 4. Race fix and write-performance design

The original background path created a fresh OS thread and Tokio runtime for
each best-effort light re-apply. It also allowed a camera/reconnect write to
overlap with a manual command at packet granularity. This was replaced with:

- one persistent coalescing worker per physical light route;
- latest-request draining so camera/reconnect/config bursts do not replay every
  intermediate state;
- generation counters that invalidate queued automatic requests when an
  explicit user command arrives;
- one async route lock covering the complete logical sequence
  (power → brightness → temperature), not just individual HID packets;
- explicit commands that cancel stale queued re-applies before writing;
- structured logs for camera transitions, applied camera-linked state, worker
  completion, worker failures, and skipped superseded requests.

This ordering guarantees that a manual command cannot be followed by the
remaining brightness/temperature packets of an already queued automatic
sequence. A later genuine camera transition is still allowed to supersede the
manual override, as required by the policy.

Primary implementation: `crates/openlogi-agent-core/src/hardware/light.rs`.

### 5. Camera-linked automation

- Added a macOS CoreMediaIO watcher using the aggregate
  `DeviceIsRunningSomewhere` property. This covers physical webcams, virtual
  cameras, capture cards, and SLR/capture devices without application-specific
  integrations.
- Added a two-consecutive-inactive-sample debounce. With the current one-second
  polling period, a transient inactive probe cannot immediately turn the light
  off; a stable inactive state is applied after roughly two seconds.
- Added `auto_camera` persistence per light.
- Camera activity changes only effective power. Persisted brightness,
  temperature, and manual preference remain independent.
- Manual power remains available while automation is enabled through a transient
  override. The override is cleared on the next real camera transition and is
  retained across parameter-only config edits.
- The aggregate camera state is appended to the agent snapshot and drives the
  GUI state/render without being written back as the persisted manual setting.

Primary files:

- `crates/openlogi-agent-core/src/watchers/camera.rs`
- `crates/openlogi-agent-core/src/orchestrator.rs`
- `crates/openlogi-gui/src/state/light.rs`
- `crates/openlogi-gui/src/components/light_panel.rs`

### 6. CLI surface

Implemented and exercised:

```text
cargo run -p openlogi -- light list
cargo run -p openlogi -- light on
cargo run -p openlogi -- light off
cargo run -p openlogi -- light brightness --percent 50
cargo run -p openlogi -- light brightness --lumens 150
cargo run -p openlogi -- light temperature --kelvin 4600
```

The CLI validates percentage bounds, native lumen bounds, and Kelvin range/step
before sending the command. A single light is selected automatically; when
multiple lights are present the CLI requires an explicit, unambiguous query.

Primary file: `crates/openlogi-cli/src/cmd/light.rs`.

### 7. GUI and visual rendering

- Added a capability-driven Light tab and controls for power, brightness, and
  colour temperature.
- Added standalone-light cards and detail rendering while preserving the
  existing HID++ keyboard RGB path.
- Added locale keys across the repository locale files.
- Added a protocol-neutral, code-rendered fallback for standalone models with
  no registered artwork.
- Registered the Litra Glow product render through a standalone driver + USB
  model lookup, separate from the HID++ asset resolver. The source and license
  status are documented beside the tracked image, outside the generated asset
  cache that packaging may clean.
- The registry-backed Glow renderer uses the verified front image directly;
  offline devices retain the existing reduced-opacity treatment.

Primary files:

- `crates/openlogi-gui/src/components/light_visual.rs`
- `crates/openlogi-gui/src/components/light_panel.rs`
- `crates/openlogi-gui/src/state/light.rs`
- `crates/openlogi-gui/src/app_assets.rs`
- `crates/openlogi-gui/src/asset.rs` and the shared registry cache

### 8. Adding a future Litra model

Glow is the only active model today, but the standalone boundary is prepared
for additional family members. A future contributor should extend the model
descriptor in `crates/openlogi-hid/src/write/litra.rs` by adding its product
ID, capability descriptor, semantic report encoding, and driver identity. The
GUI renders the generic standalone-light visual until that exact driver/model
tuple has registered artwork, and never reuses another model's product image.

The expected contribution flow is:

1. Add the product matcher and pure report goldens. If the reports differ from
   Glow, add a driver-specific encoding branch or a separate driver module;
   do not widen the HID++ path.
2. Advertise only the controls and native ranges the model really supports.
   The existing `LightCapabilities` remains the UI source of truth.
3. Set a stable `driver_id`; physical persistence continues to use the raw
   serial-backed device identity, not the model or driver name.
4. Add artwork only after its provenance and redistribution status are clear,
   then register the exact driver/vendor/product tuple in the standalone asset
   lookup. Do not pretend the raw device has an HID++ model ID.
5. Verify discovery, persistence, controls, the model-specific image, and the
   generic fallback for an unknown product ID.

This keeps support for a new family member localized to its driver descriptor,
capability/report tests, and optional artwork. The agent, raw route, config-key
pipeline, and GUI controls do not need a Glow-specific special case.

### 9. Tests and verification performed

Added or extended tests cover:

- exact Litra power, brightness, and temperature payloads;
- big-endian native values and 20-byte padding;
- invalid ranges and unsupported controls;
- raw matcher tuples and stable identity behaviour;
- standalone inventory coexistence and ordering;
- config round-trips, defaults, clamping, and camera automation;
- append-only IPC/bincode wire fixtures;
- orchestrator camera transitions and transient manual overrides;
- camera debounce cancellation and inactive confirmation;
- CLI selection and validation;
- GUI capability gating, light controls, locale parity, and registry image
  rendering;
- GUI test fixtures use an explicit memory-only configuration persistence mode,
  so realistic standalone identities cannot be written to the developer's real
  `config.toml` during `cargo test`.

The final local gate passed with rustup stable 1.97.1:

```text
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

The GUI was also rebuilt and launched with:

```text
cargo run -p openlogi-gui
```

The development bundle started with the Litra Glow selected and without a GUI
panic. Running the agent directly additionally confirmed these new structured
events in the live log:

```text
camera usage state changed active=false
applied camera-linked light state ...
light re-apply completed ...
```

The native macOS screenshot command could not capture the desktop in the
execution environment (`could not create image from display`), so no runtime
screenshot is claimed from this environment. The maintainer separately
confirmed the rendered device cards and light controls while exercising the
connected hardware.

### 10. Review resolution

The adversarial review findings were resolved in the implementation and are
covered by the automated gate and the connected-device checks above:

- **HID++ separation:** the Litra product/usage tuple is excluded from the
  generic HID++ candidate path, while other BLE HID++ devices using the same
  collection remain candidates. Regression tests cover both cases.
- **Raw identity:** serial-bearing raw devices use a physical `serial:` key;
  OS-node `id:` fallbacks are explicitly transient and cannot become persisted
  physical keys. Reconnect planning and identity tests cover the distinction.
- **Ambiguity:** duplicate indistinguishable raw nodes are rejected with
  `AmbiguousRawDevice`; transport, discovery, CLI, and GUI paths preserve that
  error instead of silently selecting one node.
- **Transient omission:** the agent keeps omitted raw nodes in a bounded
  per-node ledger and marks them offline before treating the omission as a
  detach. Recovery and grace-period tests cover the state transitions.
- **Capability-driven GUI:** power, camera automation, brightness, and
  temperature controls are rendered only from advertised capabilities and
  ranges. `LightValueRange` validates bounds, steps, units, and quantization.
- **Visible write failures:** light commands return structured IPC results;
  the GUI represents pending, accepted, and failed writes instead of hiding
  device errors behind an optimistic permanent state.
- **Wire compatibility:** standalone-device DTOs, light capabilities,
  commands, and new write errors have non-empty bincode golden fixtures, with
  the protocol version updated append-only.
- **Configuration guidance:** the standalone-light example uses a placeholder
  for the physical raw key and explains the serial requirement for reconnect
  persistence.
- **Test isolation:** `AppState` receives an explicit configuration persistence
  policy. Production uses the user file; tests use memory-only state. A full
  workspace test run was verified to leave the real configuration checksum
  unchanged.

### 11. Future extension work

These are deliberately outside the completed Glow MVP and do not require a
second device pipeline:

- add a second Litra capability/report fixture when another family model is
  available;
- add equivalent camera-state providers for Linux and Windows;
- add new artwork only after provenance and redistribution rights are clear;
- run optional packaging validation when a DMG artifact is needed.
