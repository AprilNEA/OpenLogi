# openlogi-device / openlogi-hid — the HID++ device layer

This guide covers the two-crate seam; it lives here because `openlogi-device`
owns the layer. `crates/openlogi-hid/AGENTS.md` points back here.

- The HID++ layer is split at `openlogi_device::backend::HidBackend`.
  `openlogi-device` holds everything that knows the protocol and nothing about
  a host — enumeration policy, the probe, the write layer, sessions, pairing —
  and is handed a backend. `openlogi-hid` is the backend for this machine
  (`async-hid`, the Windows composite channel, Input Monitoring, the on-disk
  probe cache) plus `host`, which supplies it to the entry points so the
  public API still reads `set_dpi(route, dpi)`. A change that makes
  `openlogi-device` depend on a host breaks CI's `wasm (portable crates)` job,
  which is the point of that job.

- `openlogi-hidpp` (lib name `hidpp`, 0BSD) is a **hard fork**, not a tracked vendor
  copy — read `crates/openlogi-hidpp/AGENTS.md` before touching that crate. Its own
  rules (protocol facts from official specs, typed wire values end to end) live there
  now, not here, to keep this file to the device-layer seam only.
- Device "kind" flows through four incompatible vocabularies (Bolt pairing register,
  feature `0x0005` `DeviceType` — defined in `openlogi-hidpp` — the assets-registry
  string, and `openlogi_core::device::DeviceKind`) — the same small integers mean
  different things in each. Never cross them by raw value; convert at the boundary.
  `kind` is identity-only. Capability decisions use a live feature-table probe or the
  last-good capabilities retained by the cache. The sole kind-derived fallback is
  `Capabilities::presumed_from_kind` for a device that has never been probed and is
  currently offline; keep it centralized and do not add new `kind` gates.
- The Agent's persistent enumerator is event-first: OS hotplug and HID++ lifecycle
  notifications are identity-free hints that request authoritative full
  re-enumeration. A named low-frequency recovery scan covers missed/unsupported events
  and non-broadcast battery features. Cache/ledger grace keeps sleeping or briefly
  unreachable devices visible. Changes to probing must preserve last-good replay,
  bounded repair, and channel retirement/reopen ordering — run the inventory/watcher
  tests and cover event bursts or loss, partial failure, and recovery, not just clean
  enumeration.
