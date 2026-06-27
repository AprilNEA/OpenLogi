# openlogi-hidpp

OpenLogi's vendored fork of the [`hidpp`](https://crates.io/crates/hidpp) crate —
an implementation of the Logitech HID++ protocol.

- **Upstream:** <https://github.com/lus/logy> (crate `hidpp`)
- **Forked at:** commit `135c5600807845c269b5d5bfa1f33032281fbd86` (upstream v0.3.0, 2025-12-26)
- **License:** 0BSD © Lukas Schulte Pelkum (see [`LICENSE`](./LICENSE))

The library target is named `hidpp`, so dependents `use hidpp::…` unchanged.
OpenLogi-specific changes live here; the source is otherwise kept close to
upstream to ease future syncs.

The crate is versioned with the OpenLogi workspace (unified versioning), not
upstream's `0.3.0` — that number is provenance, recorded above.

## Feature coverage

Beyond upstream, this fork adds typed wrappers for a broad set of HID++ 2.0
features. Each is registered in [`feature::registry`] and obtained via
`device.get_feature::<…>()`; implemented areas include:

- **Device & power** — Root, FeatureSet, DeviceInformation, DeviceTypeAndName,
  DeviceFriendlyName, UnifiedBattery, WirelessDeviceStatus.
- **Hosts & platform** — HostsInfo, ChangeHost, MultiPlatform, DualPlatform.
- **Pointer & wheel** — MousePointer, AdjustableDpi, ExtendedAdjustableDpi,
  VerticalScrolling, HiResWheel, Thumbwheel, SmartShift (and enhanced).
- **Controls & remapping** — ReprogControls (`0x1b04`, with named control-id and
  task-id constants), PersistentRemappableAction.
- **Keyboard** — Fn inversion (legacy and multi-host), DisableKeys,
  DisableKeysByUsage, ModeStatus.
- **Lighting** — Backlight, Illumination, BrightnessControl, ColorLedEffects,
  RgbEffects, PerKeyLighting.
- **Audio** — Sidetone, Equalizer.
- **Report rate** — AdjustableReportRate, ExtendedAdjustableReportRate.
- **Touch, crown & misc** — Crown, TouchpadRawXy, TouchMouseRaw,
  SolarKeyboardDashboard.

Each wrapper encodes/decodes the official wire format, models domain values with
enums/bitflags/newtypes, returns `Hidpp20Error::UnsupportedResponse` for unknown
wire values rather than guessing, and is unit-tested against the published spec.

[`feature::registry`]: ./src/feature/registry.rs
