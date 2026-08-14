---
paths:
  - "crates/openlogi-hid/**"
---

# openlogi-hid

- `openlogi-hidpp` (lib name `hidpp`, 0BSD) is a **hard fork**, not a tracked vendor
  copy — read `crates/openlogi-hidpp/AGENTS.md` before touching that crate. Its own
  rules (protocol facts from official specs, typed wire values end to end) live there
  now, not here, to keep this file to the `openlogi-hid` side only.
- Device "kind" flows through four incompatible vocabularies (Bolt pairing register,
  feature `0x0005` `DeviceType` — defined in `openlogi-hidpp` — the assets-registry
  string, and `openlogi_core::device::DeviceKind`) — the same small integers mean
  different things in each. Never cross them by raw value; convert at the boundary.
  `kind` is identity-only; capability decisions come from the feature table.
- Enumeration runs on a poll with cache/ledger grace logic so sleeping or briefly
  unreachable devices keep their identity and panels. Changes to probing must keep the
  "replay last-good inventory through transient failures" behavior intact — run the
  inventory/watcher tests and think about the partial-failure paths, not just clean
  enumeration.
