---
paths:
  - "crates/openlogi-hidpp/**"
  - "crates/openlogi-hid/**"
---

# HID++ layers

- `openlogi-hidpp` is a vendored fork of the `hidpp` crate (lib name `hidpp`, 0BSD).
  It deliberately does not inherit the workspace lints — keep its upstream-derived
  style. Document protocol facts from the official Logitech HID++ feature specs rather
  than guessing byte layouts; offsets that were reverse-engineered are marked as such
  in comments — keep those marks honest.
- Feature wrappers are typed end to end: the registry data-macro + `FeatureEndpoint`
  pattern, `num_enum` for discriminants, `bitflags` with `from_bits_retain` where
  unknown bits are legal. Unknown wire values surface as errors
  (`UnsupportedResponse`-style), never as silent defaults.
- Device "kind" flows through four incompatible vocabularies (Bolt pairing register,
  feature `0x0005` `DeviceType`, the assets-registry string, and
  `openlogi_core::device::DeviceKind`) — the same small integers mean different things
  in each. Never cross them by raw value; convert at the boundary. `kind` is
  identity-only; capability decisions come from the feature table.
- Enumeration runs on a poll with cache/ledger grace logic so sleeping or briefly
  unreachable devices keep their identity and panels. Changes to probing must keep the
  "replay last-good inventory through transient failures" behavior intact — run the
  inventory/watcher tests and think about the partial-failure paths, not just clean
  enumeration.
