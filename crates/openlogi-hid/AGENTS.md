# openlogi-hid — the host-side HID backend

The `HidBackend` implementation for this machine: `async-hid` transport, the
Windows composite channel, macOS Input Monitoring, the on-disk probe cache,
plus `host`, which supplies it to `openlogi-device`'s entry points.

The layer's contract — the `HidBackend` seam, the device-kind vocabularies,
enumeration grace — is [`crates/openlogi-device/AGENTS.md`](../openlogi-device/AGENTS.md);
read it before changing this crate. Serde types here ride the IPC wire
([`crates/openlogi-ipc/AGENTS.md`](../openlogi-ipc/AGENTS.md)), and the
cfg-gated platform code falls under
[`.claude/rules/cross-platform.md`](../../.claude/rules/cross-platform.md).
