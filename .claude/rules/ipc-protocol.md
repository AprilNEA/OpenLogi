---
paths:
  - "crates/openlogi-agent-core/**"
  - "crates/openlogi-agent/**"
---

# Agent IPC — the wire format is append-only

The GUI and agent speak tarpc over bincode on an `interprocess` local socket
(`openlogi-agent-core/src/ipc.rs`). bincode encodes the enum **variant index** and
tarpc encodes the **method order**, so the wire format is positional:

- Service methods are append-only; never reorder or remove. `protocol_version` must
  remain method 0 forever — the takeover handshake probes it across versions.
- serde enums that cross the IPC boundary are append-only too. serde encodes the
  declaration index, NOT a `#[repr(u8)]` discriminant — the two can disagree.
- Any wire change bumps `PROTOCOL_VERSION` (checked strict-equal at connect) and
  regenerates the golden tests in `crates/openlogi-agent-core/tests/wire_format.rs`,
  including the pinned-version assertion — the failure message prints the bytes.
- The goldens use tokio-serde's `Bincode::default()` = bincode `DefaultOptions`
  (varint, little-endian); the free `bincode::serialize` functions are fixint and do
  NOT produce matching bytes.
- Debug-build agents never take over a running release agent — that is by design (a
  dev agent must not displace the user's production agent), not a bug.
