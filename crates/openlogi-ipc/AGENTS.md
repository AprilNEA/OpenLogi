# openlogi-ipc — the wire format is append-only

The GUI and agent speak tarpc over bincode on an `interprocess` local socket
(`openlogi-ipc/src/ipc.rs`). bincode encodes the enum **variant index** and
tarpc encodes the **method order**, so the wire format is positional:

- Service methods are append-only; never reorder or remove. `protocol_version` must
  remain method 0 forever — the takeover handshake probes it across versions.
- serde enums that cross the IPC boundary are append-only too. serde encodes the
  declaration index, NOT a `#[repr(u8)]` discriminant — the two can disagree. The wire
  surface is wider than this crate: serde types from `openlogi-core` (device model,
  `DeviceKind`, actions, config) and `openlogi-hid` (write errors) ride inside RPC
  payloads, so the same rule binds them.
- Any wire change bumps `PROTOCOL_VERSION` (checked strict-equal at connect) and
  regenerates the golden tests in `crates/openlogi-ipc/tests/wire_format.rs`,
  including the pinned-version assertion — the failure message prints the bytes
  (`left` is the new encoding). Run `cargo test -p openlogi-ipc --test wire_format`
  before any push that touched wire types.
- The goldens use tokio-serde's `Bincode::default()` = bincode `DefaultOptions`
  (varint, little-endian); the free `bincode::serialize` functions are fixint and do
  NOT produce matching bytes.
- Debug-build agents never take over a running release agent — that is by design (a
  dev agent must not displace the user's production agent), not a bug.

## The client policy lives here too

`src/client.rs` owns everything a client must do identically: `connect_as(kind)` is
the handshake (connect, judge the version in both directions, declare — all within
`HANDSHAKE_TIMEOUT`, so no caller adds a timeout of its own),
`probe_version` the agent's takeover probe, and `Ledger` and `observe_context` the
observe bookkeeping (the thread a client loop runs on is `openlogi_core::runtime`'s).
Consumers
never compare `PROTOCOL_VERSION`, call `declare_client`, or open the transport
themselves — the `.ast-grep/rules/ipc-*.yml` guards fail the `ast-grep` CI job on any
of that outside this crate. A new decision every client must share goes here, with
its guard, not into the first client that needs it. `testing::in_memory_agent`
(feature `test-support`) is the scripted agent for client tests.
